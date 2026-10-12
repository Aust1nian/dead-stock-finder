//! Tokens live in the operating system's keychain and nowhere else: never
//! in files, logs, exports or diagnostics. `Debug` on [`Tokens`] prints no
//! secret, and the property tests scan every file the app writes for them.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_token: String,
    pub refresh_expires_at: Option<DateTime<Utc>>,
    /// When this access token was issued (by this computer's clock).
    pub obtained_at: DateTime<Utc>,
    pub merchant_id: String,
    pub scopes: Vec<String>,
    pub environment: String,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("obtained_at", &self.obtained_at)
            .field("merchant_id", &self.merchant_id)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The keychain wanted to ask the owner something (it is locked, or
    /// another build of the app made the entry), and this run may not show
    /// anything. The next check, or opening the app, tries again.
    #[error("the keychain needs the owner's attention")]
    NeedsInteraction,
    #[error("keychain error: {0}")]
    Os(String),
    #[error("the saved connection is unreadable")]
    Corrupt,
}

pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<Tokens>, StoreError>;
    fn save(&self, tokens: &Tokens) -> Result<(), StoreError>;
    fn delete(&self) -> Result<(), StoreError>;
}

/// For tests and the self-test: tokens held in memory only.
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<Tokens>>);

impl MemoryStore {
    pub fn with(tokens: Tokens) -> Self {
        MemoryStore(Mutex::new(Some(tokens)))
    }
}

impl TokenStore for MemoryStore {
    fn load(&self) -> Result<Option<Tokens>, StoreError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, tokens: &Tokens) -> Result<(), StoreError> {
        *self.0.lock().unwrap() = Some(tokens.clone());
        Ok(())
    }
    fn delete(&self) -> Result<(), StoreError> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

/// The operating system's keychain: macOS Keychain, Windows Credential
/// Manager, and on Linux (development only) the kernel's per-user keyring.
pub struct OsKeychain {
    service: String,
    account: String,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    interactive: bool,
}

impl OsKeychain {
    /// `interactive: false` for background checks, which must never show a
    /// keychain prompt; a lookup that would prompt fails instead.
    pub fn new(service: &str, environment: &str, interactive: bool) -> Self {
        OsKeychain { service: service.to_string(), account: format!("square-{environment}"), interactive }
    }
}

fn encode(tokens: &Tokens) -> Vec<u8> {
    serde_json::to_vec(tokens).expect("tokens serialize")
}

fn decode(bytes: &[u8]) -> Result<Tokens, StoreError> {
    serde_json::from_slice(bytes).map_err(|_| StoreError::Corrupt)
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password_options, generic_password, set_generic_password_options,
    };
    use security_framework_sys::base::errSecItemNotFound;

    /// Not exported by security-framework-sys.
    const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

    unsafe extern "C" {
        // Security.framework; deprecated in favour of LAContext, which
        // doesn't cover the file-based login keychain this uses.
        static kSecUseAuthenticationUIFail: core_foundation::string::CFStringRef;
    }

    fn options(k: &OsKeychain) -> PasswordOptions {
        let mut options = PasswordOptions::new_generic_password(&k.service, &k.account);
        if !k.interactive {
            // kSecUseAuthenticationUI = kSecUseAuthenticationUIFail: fail
            // instead of showing a keychain prompt from a background check.
            #[allow(deprecated)]
            unsafe {
                options.query.push((
                    CFString::wrap_under_get_rule(security_framework_sys::item::kSecUseAuthenticationUI),
                    CFString::wrap_under_get_rule(kSecUseAuthenticationUIFail).into_CFType(),
                ));
            }
        }
        options
    }

    fn map(e: security_framework::base::Error) -> StoreError {
        if e.code() == ERR_SEC_INTERACTION_NOT_ALLOWED {
            StoreError::NeedsInteraction
        } else {
            StoreError::Os(e.to_string())
        }
    }

    pub fn load(k: &OsKeychain) -> Result<Option<Tokens>, StoreError> {
        match generic_password(options(k)) {
            Ok(bytes) => decode(&bytes).map(Some),
            Err(e) if e.code() == errSecItemNotFound => Ok(None),
            Err(e) => Err(map(e)),
        }
    }

    pub fn save(k: &OsKeychain, tokens: &Tokens) -> Result<(), StoreError> {
        let mut options = options(k);
        options.set_label("Dead Stock Finder: Square connection");
        set_generic_password_options(&encode(tokens), options).map_err(map)
    }

    pub fn delete(k: &OsKeychain) -> Result<(), StoreError> {
        match delete_generic_password_options(options(k)) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == errSecItemNotFound => Ok(()),
            Err(e) => Err(map(e)),
        }
    }
}

#[cfg(windows)]
mod os {
    use super::*;
    use std::ptr;
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
        CredWriteW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn target(k: &OsKeychain) -> Vec<u16> {
        wide(&format!("{}/{}", k.service, k.account))
    }

    pub fn load(k: &OsKeychain) -> Result<Option<Tokens>, StoreError> {
        let target = target(k);
        let mut cred: *mut CREDENTIALW = ptr::null_mut();
        let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return if err == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(StoreError::Os(format!("CredReadW {err}")))
            };
        }
        let bytes = unsafe {
            let c = &*cred;
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
        };
        unsafe { CredFree(cred as *const _) };
        decode(&bytes).map(Some)
    }

    pub fn save(k: &OsKeychain, tokens: &Tokens) -> Result<(), StoreError> {
        let mut target = target(k);
        let mut user = wide("Square");
        let mut blob = encode(tokens);
        let mut cred: CREDENTIALW = unsafe { std::mem::zeroed() };
        cred.Type = CRED_TYPE_GENERIC;
        cred.TargetName = target.as_mut_ptr();
        cred.UserName = user.as_mut_ptr();
        cred.CredentialBlobSize = blob.len() as u32;
        cred.CredentialBlob = blob.as_mut_ptr();
        cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
        let ok = unsafe { CredWriteW(&cred, 0) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(StoreError::Os(format!("CredWriteW {err}")));
        }
        Ok(())
    }

    pub fn delete(k: &OsKeychain) -> Result<(), StoreError> {
        let target = target(k);
        let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err != ERROR_NOT_FOUND {
                return Err(StoreError::Os(format!("CredDeleteW {err}")));
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::*;
    use linux_keyutils::{KeyError, KeyPermissionsBuilder, KeyRing, KeyRingIdentifier, Permission};

    fn description(k: &OsKeychain) -> String {
        format!("{}:{}", k.service, k.account)
    }

    fn ring() -> Result<KeyRing, StoreError> {
        KeyRing::from_special_id(KeyRingIdentifier::User, true).map_err(|e| StoreError::Os(format!("{e:?}")))
    }

    pub fn load(k: &OsKeychain) -> Result<Option<Tokens>, StoreError> {
        match ring()?.search(&description(k)) {
            Ok(key) => decode(&key.read_to_vec().map_err(|e| StoreError::Os(format!("{e:?}")))?).map(Some),
            Err(KeyError::KeyDoesNotExist | KeyError::KeyRevoked | KeyError::KeyExpired) => Ok(None),
            Err(e) => Err(StoreError::Os(format!("{e:?}"))),
        }
    }

    pub fn save(k: &OsKeychain, tokens: &Tokens) -> Result<(), StoreError> {
        let key = ring()?
            .add_key(&description(k), &encode(tokens))
            .map_err(|e| StoreError::Os(format!("{e:?}")))?;
        // The user, not only the possessing session, may find and read it,
        // so a later process in a new session can load it.
        let perms = KeyPermissionsBuilder::builder().posessor(Permission::ALL).user(Permission::ALL).build();
        key.set_perms(perms).map_err(|e| StoreError::Os(format!("{e:?}")))
    }

    pub fn delete(k: &OsKeychain) -> Result<(), StoreError> {
        let ring = ring()?;
        match ring.search(&description(k)) {
            Ok(key) => ring.unlink_key(key).map_err(|e| StoreError::Os(format!("{e:?}"))),
            Err(KeyError::KeyDoesNotExist | KeyError::KeyRevoked | KeyError::KeyExpired) => Ok(()),
            Err(e) => Err(StoreError::Os(format!("{e:?}"))),
        }
    }
}

impl TokenStore for OsKeychain {
    fn load(&self) -> Result<Option<Tokens>, StoreError> {
        os::load(self)
    }
    fn save(&self, tokens: &Tokens) -> Result<(), StoreError> {
        os::save(self, tokens)
    }
    fn delete(&self) -> Result<(), StoreError> {
        os::delete(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sample() -> Tokens {
        Tokens {
            access_token: "EAAAl-test-access".into(),
            access_expires_at: Utc::now() + chrono::Duration::days(30),
            refresh_token: "EQAAl-test-refresh".into(),
            refresh_expires_at: None,
            obtained_at: Utc::now(),
            merchant_id: "M1".into(),
            scopes: vec!["ITEMS_READ".into()],
            environment: "mock".into(),
        }
    }

    #[test]
    fn debug_hides_secrets() {
        let text = format!("{:?}", sample());
        assert!(!text.contains("test-access") && !text.contains("test-refresh"), "{text}");
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::default();
        assert!(store.load().unwrap().is_none());
        let tokens = sample();
        store.save(&tokens).unwrap();
        assert_eq!(store.load().unwrap(), Some(tokens));
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    /// The real keychain: a second store object (as after a restart) reads
    /// what the first saved. Skips where the OS keychain isn't available.
    #[test]
    fn os_keychain_survives_a_new_handle() {
        let service = format!("com.tightline.deadstockfinder.test.{}", std::process::id());
        let first = OsKeychain::new(&service, "mock", true);
        let tokens = sample();
        if let Err(e) = first.save(&tokens) {
            eprintln!("keychain unavailable here, skipping: {e}");
            return;
        }
        let second = OsKeychain::new(&service, "mock", false);
        assert_eq!(second.load().unwrap(), Some(tokens));
        second.delete().unwrap();
        assert!(first.load().unwrap().is_none());
        first.delete().unwrap();
    }
}
