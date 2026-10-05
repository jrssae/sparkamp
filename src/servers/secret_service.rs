//! Server passwords in the desktop keyring on Linux: GNOME Keyring, KWallet,
//! KeePassXC or any other Secret Service provider, over the session D-Bus.
//!
//! zbus is already linked for udisks, so this adds no crate. The part of the
//! protocol a password store needs is small: open a session, find an item by
//! its attributes, read it, write it, delete it, and answer the keyring's
//! unlock prompt when it asks for one.
//!
//! Secrets cross the bus in the protocol's "plain" transfer mode. The session
//! bus belongs to the logged-in user and only the keyring daemon is on the
//! other end; the encrypted mode protects against the bus itself being
//! logged, which costs a Diffie-Hellman implementation this file does not
//! carry. The password never reaches Sparkamp's config or logs either way.
//!
//! Inside the Flatpak this needs `--talk-name=org.freedesktop.secrets` (see
//! `dev.sparkamp.Sparkamp.yml`). Where no keyring answers at all, a headless
//! box or a session without one, [`super::manager::platform_secrets`] falls
//! back to holding passwords for the session only.

use super::manager::SecretStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type, Value};

const DEST: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
/// The user's default keyring ("Login" in GNOME, "kdewallet" in KDE).
const DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const PROMPT_IFACE: &str = "org.freedesktop.Secret.Prompt";

/// The attribute libsecret files an item's schema under, so the item reads as
/// Sparkamp's in Seahorse and KWallet Manager.
const SCHEMA: &str = "dev.sparkamp.Sparkamp.Server";

/// How long one keyring call may take before it is given up on. Generous,
/// since the first call may start the keyring daemon; a call that waits on an
/// unlock prompt is not bound by it (see [`SecretServiceSecrets::prompt`]).
const CALL_TIMEOUT: Duration = Duration::from_secs(25);

/// The protocol's secret: which session it travels in, the transfer mode's
/// parameters (none for "plain"), the bytes, and their type.
#[derive(Serialize, Deserialize, Type)]
struct Secret {
    session: OwnedObjectPath,
    parameters: Vec<u8>,
    value: Vec<u8>,
    content_type: String,
}

/// Passwords in the desktop keyring, one item per server id.
pub struct SecretServiceSecrets {
    conn: Connection,
}

impl SecretServiceSecrets {
    /// Connect to the session bus and check a keyring answers there. An
    /// error means there is none to use.
    pub fn connect() -> zbus::Result<Self> {
        let conn = zbus::blocking::connection::Builder::session()?.method_timeout(CALL_TIMEOUT).build()?;
        let store = SecretServiceSecrets { conn };
        // Opening a session is the cheapest call that proves a provider is
        // there, and it starts one that is only D-Bus activatable.
        store.open_session()?;
        Ok(store)
    }

    fn service(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(&self.conn, DEST, SERVICE_PATH, SERVICE_IFACE)
    }

    fn open_session(&self) -> zbus::Result<OwnedObjectPath> {
        let (_, session): (OwnedValue, OwnedObjectPath) =
            self.service()?.call("OpenSession", &("plain", Value::from("")))?;
        Ok(session)
    }

    /// The attributes that pick out `server_id`'s item.
    fn attributes(server_id: &str) -> HashMap<&str, &str> {
        HashMap::from([("xdg:schema", SCHEMA), ("server-id", server_id)])
    }

    /// Every item stored for `server_id`, unlocked first.
    fn find(&self, server_id: &str) -> zbus::Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)> {
        self.service()?.call("SearchItems", &(Self::attributes(server_id),))
    }

    /// Answer a prompt the keyring handed back: show it and wait for the
    /// user. `/` means no prompt was needed. Returns whether the user went
    /// through with it rather than dismissing it.
    ///
    /// The wait is the user's to end, so it carries no timeout: the prompt
    /// is the keyring's own dialog and stays up until answered.
    fn prompt(&self, prompt: &OwnedObjectPath) -> zbus::Result<bool> {
        if prompt.as_str() == "/" {
            return Ok(true);
        }
        let proxy = Proxy::new(&self.conn, DEST, prompt.as_str(), PROMPT_IFACE)?;
        // Listen before asking, or a quick answer is missed.
        let mut completed = proxy.receive_signal("Completed")?;
        proxy.call_method("Prompt", &("",))?;
        match completed.next() {
            Some(message) => {
                let (dismissed, _result): (bool, OwnedValue) = message.body().deserialize()?;
                Ok(!dismissed)
            }
            None => Ok(false),
        }
    }

    fn read(&self, server_id: &str) -> anyhow::Result<Option<String>> {
        let (unlocked, locked) = self.find(server_id)?;
        let item = match (unlocked.into_iter().next(), locked.into_iter().next()) {
            (Some(item), _) => item,
            (None, Some(item)) => {
                let (done, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
                    self.service()?.call("Unlock", &(vec![item.clone()],))?;
                if done.is_empty() && !self.prompt(&prompt)? {
                    anyhow::bail!("the keyring stayed locked");
                }
                item
            }
            (None, None) => return Ok(None),
        };
        let session = self.open_session()?;
        let secret: Secret =
            Proxy::new(&self.conn, DEST, item.as_str(), ITEM_IFACE)?.call("GetSecret", &(session,))?;
        Ok(Some(String::from_utf8(secret.value)?))
    }

    fn write(&self, server_id: &str, password: &str) -> anyhow::Result<()> {
        let session = self.open_session()?;
        let attributes: HashMap<String, String> =
            Self::attributes(server_id).into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let properties: HashMap<&str, Value> = HashMap::from([
            ("org.freedesktop.Secret.Item.Label", Value::from("Sparkamp music server password")),
            ("org.freedesktop.Secret.Item.Attributes", Value::from(attributes)),
        ]);
        let secret = Secret {
            session,
            parameters: Vec::new(),
            value: password.as_bytes().to_vec(),
            content_type: "text/plain; charset=utf8".to_string(),
        };
        let collection = Proxy::new(&self.conn, DEST, DEFAULT_COLLECTION, COLLECTION_IFACE)?;
        // `replace`: a server's password is changed by storing it again.
        let (item, prompt): (OwnedObjectPath, OwnedObjectPath) =
            collection.call("CreateItem", &(properties, secret, true))?;
        // A locked keyring answers with a prompt to unlock it instead of the
        // item, and stores the secret once the user unlocks.
        if item.as_str() == "/" && !self.prompt(&prompt)? {
            anyhow::bail!("the keyring stayed locked");
        }
        Ok(())
    }

    fn remove(&self, server_id: &str) -> anyhow::Result<()> {
        let (unlocked, locked) = self.find(server_id)?;
        for item in unlocked.into_iter().chain(locked) {
            let prompt: OwnedObjectPath =
                Proxy::new(&self.conn, DEST, item.as_str(), ITEM_IFACE)?.call("Delete", &())?;
            if !self.prompt(&prompt)? {
                anyhow::bail!("the keyring stayed locked");
            }
        }
        Ok(())
    }
}

impl SecretStore for SecretServiceSecrets {
    fn get(&self, server_id: &str) -> Option<String> {
        match self.read(server_id) {
            Ok(password) => password,
            // Stored but not readable (refused, locked, keyring gone): worth
            // a line, since it reads as "no password" otherwise. The error
            // names the call that failed, never the secret.
            Err(e) => {
                eprintln!("[servers] {server_id}: the keyring did not give the password: {e:#}");
                None
            }
        }
    }
    fn set(&self, server_id: &str, secret: &str) -> anyhow::Result<()> {
        self.write(server_id, secret).map_err(|e| anyhow::anyhow!("keyring: {e:#}"))
    }
    fn delete(&self, server_id: &str) -> anyhow::Result<()> {
        self.remove(server_id).map_err(|e| anyhow::anyhow!("keyring: {e:#}"))
    }
    fn persistent(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real desktop keyring, so it only runs when asked:
    /// `cargo test -- --ignored secret_service`.
    #[test]
    #[ignore]
    fn secret_service_round_trips_a_password() {
        let k = SecretServiceSecrets::connect().expect("a keyring on the session bus");
        let id = format!("sparkamp-test-{}", std::process::id());
        k.set(&id, "sesame").unwrap();
        assert_eq!(k.get(&id).as_deref(), Some("sesame"));
        k.set(&id, "changed").unwrap();
        assert_eq!(k.get(&id).as_deref(), Some("changed"), "storing again replaces");
        k.delete(&id).unwrap();
        assert_eq!(k.get(&id), None);
        k.delete(&id).unwrap();
    }
}
