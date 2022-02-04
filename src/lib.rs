#[macro_use]
extern crate ic_cdk_macros;
#[macro_use]
extern crate serde;

use std::collections::{HashSet, BTreeSet};

use ic_cdk::{export::candid, storage, api::{self, call::{self, RejectionCode}}};
use candid::{Principal, CandidType};
use once_cell::sync::Lazy;
use parking_lot::{RwLock, MappedRwLockReadGuard, RwLockReadGuard};

mod http;
mod wrap;

use wrap::Wrapper;

#[init]
fn init(custodians: Option<HashSet<Principal>>) {
    STORAGE.write().custodians = custodians.unwrap_or_else(|| HashSet::from([api::caller()]));
    http::bake_hashes();
}

#[pre_upgrade]
fn pre_upgrade() {
    storage::stable_save((&*STORAGE.read(),)).unwrap();
}

#[post_upgrade]
fn post_upgrade() {
    let (s,): (Storage,) = storage::stable_restore().unwrap();
    *STORAGE.write() = s;
    http::bake_hashes();
}

#[derive(CandidType, Deserialize, Ord, PartialOrd, Eq, PartialEq, Clone, Copy)]
struct Nft {
    canister: Principal,
    index: u64,
}

#[derive(CandidType, Deserialize)]
enum Error {
    InvalidCanister,
    CannotNotify,
    CanisterError {
        message: String,
    },
    NoSuchToken,
    NotOwner,
    Unauthorized,
}

impl From<DipError> for Error {
    fn from(e: DipError) -> Self {
        match e {
            DipError::InvalidTokenId => Self::NoSuchToken,
            DipError::Unauthorized => Self::NotOwner,
            _ => Self::CanisterError { message: format!("{e:?}") },
        }
    }
}

type Result<T = (), E = Error> = std::result::Result<T, E>;

impl From<(RejectionCode, String)> for Error {
    fn from((code, message): (RejectionCode, String)) -> Self {
        match code {
            RejectionCode::CanisterError => Self::CanisterError { message },
            _ => Self::InvalidCanister,
        }
    }
}

#[inspect_message]
fn inspect_message() {
    if is_authorized() || !["set_authorized", "transfer", "register"].contains(&call::method_name().as_str()) {
        call::accept_message();
    }
}

#[query]
fn is_authorized() -> bool {
    STORAGE.read().custodians.contains(&api::caller())
}

#[update]
fn set_authorized(principal: Principal, authorized: bool) -> Result {
    if !is_authorized() {
        return Err(Error::Unauthorized);
    }
    if authorized {
        STORAGE.write().custodians.insert(principal);
    } else {
        STORAGE.write().custodians.remove(&principal);
    }
    Ok(())
}

#[derive(CandidType, Deserialize, Debug)]
enum DipError {
    Unauthorized,
    InvalidTokenId,
    ZeroAddress,
    Other,
}

#[update]
async fn register(nft: Nft) -> Result {
    if !is_authorized() {
        return Err(Error::Unauthorized);
    }
    if STORAGE.read().owned_nfts.contains(&nft) {
        return Ok(());
    }
    if let Ok((owner,)) = call::call::<_, (Result<Principal, DipError>,)>(nft.canister, "ownerOfDip721", (nft.index,)).await {
        if !matches!(owner, Ok(p) if p == api::id()) {
            return Err(Error::NotOwner);
        }
    } else {
        return Err(Error::InvalidCanister);
    }
    STORAGE.write().owned_nfts.insert(nft);
    Ok(())
}

#[update]
async fn burn(nft: Nft) -> Result {
    if !is_authorized() {
        return Err(Error::Unauthorized);
    }
    call::call::<_, (Result<u128, DipError>,)>(nft.canister, "burnDip721", (nft.index,)).await?.0?;
    Ok(())
}

#[query]
fn owned_nfts() -> Wrapper<MappedRwLockReadGuard<'static, BTreeSet<Nft>>> {
    Wrapper(RwLockReadGuard::map(STORAGE.read(), |s| &s.owned_nfts))
}

#[update]
async fn transfer(nft: Nft, target: Principal, notify: Option<bool>) -> Result {
    if !is_authorized() {
        return Err(Error::Unauthorized);
    }
    if !STORAGE.read().owned_nfts.contains(&nft) {
        register(nft).await?;
    }
    if notify != Some(false) {
        if let Ok((res,)) = call::call::<_, (Result<u128, DipError>,)>(nft.canister, "safeTransferFromNotifyDip721", (api::id(), target, nft.index, Vec::<u8>::new())).await {
            res?;
        } else {
            if notify == None {
                call::call::<_, (Result<u128, DipError>,)>(nft.canister, "safeTransferFromDip721", (api::id(), target, nft.index)).await?.0?;
            } else {
                return Err(Error::CannotNotify);
            }
        }
    } else {
        call::call::<_, (Result<u128, DipError>,)>(nft.canister, "safeTransferFromDip721", (api::id(), target, nft.index)).await?.0?;
    }
    STORAGE.write().owned_nfts.remove(&nft);
    Ok(())
}

#[update(name = "onDIP721Received")]
fn on_dip721_received(_: Principal, _: Principal, tokenid: u64, _: Vec<u8>) {
    STORAGE.write().owned_nfts.insert(Nft { canister: api::caller(), index: tokenid });
}

#[derive(CandidType, Deserialize, Default)]
struct Storage {
    custodians: HashSet<Principal>,
    owned_nfts: BTreeSet<Nft>, // more to come following inter-canister queries
}

#[update]
fn wallet_receive() {
    call::msg_cycles_accept(1_000_000_000_000_u128.saturating_sub(api::canister_balance128()) as u64);
}

static STORAGE: Lazy<RwLock<Storage>> = Lazy::new(|| RwLock::new(Storage::default()));
