//! The one thing the server's page may ask the native shell to do.
//!
//! Arciin's My Computers page needs to open the native "Protect folders from
//! this PC" screen, because only the desktop can see Windows folders. That is
//! the entire purpose of this module, and deliberately its entire capability.
//!
//! # Why this is not an RPC channel
//!
//! The page rendering in that WebView comes from a server over the network. If
//! this were a general "run a native command" bridge, anyone who could inject
//! a script into that page — or a compromised server, or a stale cached
//! bundle — would be running native code on the user's machine. So there is no
//! command name, no method dispatch, and nowhere to put an argument.
//!
//! # Why a navigation rather than a web message
//!
//! The first version of this contract was a `postMessage` carrying a fixed
//! three-field message. It cannot work here: under Tauri 2.11 / wry 0.55 the
//! page's `chrome.webview.postMessage` is a wrapper — `postMessage.name`
//! reads `"bound postMessage"` — that feeds Tauri's own IPC instead of
//! WebView2's host, so a native `add_WebMessageReceived` handler is never
//! called. That was proved from the content webview itself: the post succeeds,
//! the handler never runs.
//!
//! A navigation is the smallest thing that does cross, and it is strictly
//! *less* capable than a message channel. The app already inspects every
//! navigation to decide what may load, so this recognises one more target in a
//! check that was happening anyway: no new surface, no new parser, no payload.
//!
//! Contract: `arciin-main/packages/shared/src/native-bridge.ts`.

/// A navigation that passed every check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAction {
    OpenComputerBackupSetup,
}

/// The scheme the page navigates to when it wants the native setup screen.
pub const SENTINEL_SCHEME: &str = "arciin-native";

/// The one navigation target that means anything to the shell.
///
/// Matches `ARCIIN_NATIVE_BACKUP_SETUP_URL` in the server's shared package.
pub const SETUP_SENTINEL: &str = "arciin-native://backup/setup";

/// Decide whether a navigation is the one action we accept.
///
/// Deliberately unforgiving. The whole value of a sentinel over a message is
/// that it cannot carry anything: a query string, a fragment, embedded
/// credentials, a port, or any other host or path means this is not the
/// sentinel, and it is refused rather than sanitised. There is no id, no name
/// and no path in the accepted form — only the fact that it was navigated to.
pub fn classify_navigation(target: &url::Url) -> Option<NativeAction> {
    if target.scheme() != SENTINEL_SCHEME {
        return None;
    }

    // Nothing may ride along.
    if target.query().is_some()
        || target.fragment().is_some()
        || !target.username().is_empty()
        || target.password().is_some()
        || target.port().is_some()
    {
        return None;
    }

    match (target.host_str(), target.path()) {
        (Some("backup"), "/setup") => Some(NativeAction::OpenComputerBackupSetup),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nav(target: &str) -> Option<NativeAction> {
        classify_navigation(&url::Url::parse(target).unwrap())
    }

    #[test]
    fn the_sentinel_opens_the_setup_screen() {
        assert_eq!(
            nav(SETUP_SENTINEL),
            Some(NativeAction::OpenComputerBackupSetup)
        );
    }

    #[test]
    fn the_sentinel_matches_the_servers_constant() {
        // If the server's URL and this constant ever drift, the button goes
        // quiet with no error anywhere. Pinned here so a change has to be
        // deliberate on both sides.
        assert_eq!(SETUP_SENTINEL, "arciin-native://backup/setup");
    }

    #[test]
    fn the_sentinel_cannot_carry_a_payload() {
        // The reason a navigation is acceptable at all: there is nowhere to
        // put a path, a command or a credential. Anything appended makes it a
        // different URL, and a different URL is not the sentinel.
        for target in [
            "arciin-native://backup/setup?path=C:%5CWindows",
            "arciin-native://backup/setup#cmd",
            "arciin-native://user:secret@backup/setup",
            "arciin-native://backup:8080/setup",
            "arciin-native://backup/setup/../../etc",
        ] {
            assert_eq!(nav(target), None, "{target} must be refused");
        }
    }

    #[test]
    fn no_other_sentinel_target_is_recognised() {
        // A guard against this growing into an RPC channel by accretion.
        for target in [
            "arciin-native://backup/enable",
            "arciin-native://backup/disable",
            "arciin-native://shell/run",
            "arciin-native://files/read",
            "arciin-native://backup",
            "arciin-native://backup/SETUP",
        ] {
            assert_eq!(nav(target), None, "{target} must be refused");
        }
    }

    #[test]
    fn another_scheme_is_not_a_sentinel() {
        // Real links must stay real links; this must not swallow them.
        for target in [
            "http://192.168.1.50:3002/computers",
            "https://arciin.example/settings",
            "arciin://backup/setup",
            "arciin-native-x://backup/setup",
            "javascript:alert(1)",
            "file:///C:/Windows/System32",
        ] {
            assert_eq!(nav(target), None, "{target} must not be a sentinel");
        }
    }

    #[test]
    fn there_is_exactly_one_action() {
        let mut accepted = 0;
        for target in [
            SETUP_SENTINEL,
            "arciin-native://backup/anything",
            "arciin-native://run/exec",
            "arciin-native://setup/backup",
        ] {
            if nav(target).is_some() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 1, "exactly one target may ever be accepted");
    }
}
