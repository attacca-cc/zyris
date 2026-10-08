//! What this run calls itself on this machine, and where everything it owns lives.
//!
//! **One string, three uses**: the keychain service the credential is stored under, the name of
//! the instance lock, and the directory the audit log, the peer key, the MCP server list, the
//! voice settings and the console's state file all live in. Derived in one place so they cannot
//! disagree — an audit log written under one name and a lock taken under another is a machine
//! whose history belongs to nobody.
//!
//! A console command needs the same two answers as the node does, which is why this is a module
//! rather than two functions in `main`: `zyris config` has to edit the settings of *this*
//! instance, and `zyris status` has to ask about the same lock the node took.

use std::path::PathBuf;

/// What this run calls itself on this machine: the keychain service, the instance lock's name,
/// and the directory the state files land in. All three from one string, so they cannot disagree.
///
/// A default run is `zyris`, exactly as it has always been, so nothing about an existing install
/// moves. A `--server` run earns a name of its own, because it is a different node: its
/// credentials belong to that server, its calls are not this machine's real history, and it has
/// to be able to run *beside* a production instance rather than be turned away by its lock —
/// which is the point of the flag.
///
/// Naming the lock is only half of that. The other half is in `gui.rs`: `tauri-plugin-single-
/// instance` keys on the bundle identifier rather than on this name, so a windowed `--server`
/// run skips registering it. Both halves are needed, and neither works alone.
///
/// Every character that is not `[0-9A-Za-z]` is replaced, so the result is usable as a directory
/// name, a file name and a keychain service on every platform. Two servers that differ only in
/// punctuation collide into one name; that is chosen over hashing, because a person who finds
/// `zyris-dev-ws---127-0-0-1-8080-zyris-v1-ws` in their data directory can tell what it is.
pub fn name(server: Option<&str>) -> String {
    match server {
        None => "zyris".to_string(),
        Some(url) => {
            format!("zyris-dev-{}", url.replace(|c: char| !c.is_ascii_alphanumeric(), "-"))
        }
    }
}

/// Where everything this run owns on disk lives: beside the other per-user state, never beside
/// the binary. The audit log, this machine's peer key, the ledger of peers it has pinned, the
/// inbox, the MCP server list, the voice settings and the console's state file are all under here.
///
/// Scoped by the instance for the same reason the keychain is — a `--server` run must not append
/// its calls to the production machine's history, nor read that history back as its own, nor
/// answer to the production machine's peer identity.
///
/// The fallbacks mirror `SecretStore`'s, and for the same reason — the current directory is `/`
/// under a systemd unit and whatever a shortcut set for a desktop launch, so anything written
/// there lands somewhere different every launch. The last resort is a directory rather than a
/// prefixed file name, because there is now more than one file to put in it.
pub fn data_dir(instance: &str) -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("cc", "attacca", instance) {
        return dirs.data_dir().to_path_buf();
    }
    if let Some(dirs) = directories::BaseDirs::new() {
        return dirs.home_dir().join(format!(".{instance}"));
    }
    // No directory the platform can name. `AuditLog` survives a path it cannot write — it says
    // so in the process log and never fails a tool call — so an absolute, OS-chosen path is a
    // better last resort than refusing to start.
    std::env::temp_dir().join(instance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_instance_keeps_the_name_an_existing_install_already_uses() {
        // This exact string is the keychain service, the lock's name and the audit directory on
        // every machine already running Zyris. Changing it orphans their stored credentials and
        // enrols the machine again on the next launch.
        assert_eq!(name(None), "zyris");
    }

    #[test]
    fn a_server_run_is_a_different_instance_from_the_default_one() {
        // Otherwise a development run reads and writes the production credential — and a dev
        // server that refuses it makes this app forget it.
        assert_ne!(name(None), name(Some("ws://127.0.0.1:8080/zyris/v1/ws")));
    }

    #[test]
    fn two_servers_are_two_instances() {
        assert_ne!(
            name(Some("ws://127.0.0.1:8080/zyris/v1/ws")),
            name(Some("ws://127.0.0.1:9090/zyris/v1/ws"))
        );
    }

    #[test]
    fn an_instance_name_is_safe_as_a_file_name_and_as_a_keychain_service() {
        let name = name(Some("ws://127.0.0.1:8080/zyris/v1/ws"));

        assert!(
            name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "a name carrying a path separator would land the log somewhere else entirely: {name}"
        );
    }

    #[test]
    fn each_instance_reads_its_own_mcp_server_list() {
        // The list of MCP servers is per-instance like everything else this run owns, and for the
        // same reason: a `--server` run must not start the production machine's servers and
        // announce them to a development server, nor the other way round. `Config::path` takes a
        // directory rather than finding one so that this is decided once, where the instance is.
        let production = zyris_mcp::Config::path(&data_dir(&name(None)));
        let development =
            zyris_mcp::Config::path(&data_dir(&name(Some("ws://127.0.0.1:8080/zyris/v1/ws"))));

        assert_ne!(production, development);
        // And it lands beside the rest of that instance's state rather than beside the binary or
        // in whatever directory Zyris happened to be started from.
        assert_eq!(production.parent().unwrap(), data_dir("zyris"));
    }
}
