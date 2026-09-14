//! The Windows "choose a folder" dialog.
//!
//! Known folders cover what most people mean by "my stuff", but not a specific
//! project directory on a D: drive. This is the escape hatch, and it is
//! deliberately the *only* way a path that is not a known folder can enter the
//! app.
//!
//! # Why the path never leaves Rust
//!
//! A Windows path is personal — it usually carries the account name, and often
//! a project or client name too. So the path chosen here is kept in native
//! memory, handed to the scanner, and never sent anywhere: not to the server
//! (the protocol takes an opaque `sourcePathIdentifier` instead), not to the
//! renderer, and certainly not to the page the server serves.
//!
//! # Why this cannot be driven remotely
//!
//! The dialog is opened by a Tauri command, and Tauri commands are reachable
//! only from webviews covered by a capability. `arciin-content` — the webview
//! showing the server's page — is in none, so it has no IPC at all. The only
//! caller is the native Protect Folders screen, and even it cannot *name* a
//! folder: it can only ask for the dialog, which a person then operates.

use std::path::PathBuf;

/// Show the folder picker and wait for an answer.
///
/// `parent` is the window handle to parent the dialog to, so it behaves like a
/// modal of the app rather than a stray window. Returns `None` when the person
/// cancels, which is an ordinary outcome and not an error.
pub fn pick_folder(parent: Option<isize>) -> Option<PathBuf> {
    imp::pick_folder(parent)
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;

    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };

    /// Run the dialog on a thread of its own.
    ///
    /// `IFileOpenDialog` needs a single-threaded apartment, and it pumps its
    /// own message loop while open. Giving it a dedicated thread keeps it from
    /// blocking the loop that drives the app's windows, so the rest of the app
    /// keeps painting while the picker is up.
    pub fn pick_folder(parent: Option<isize>) -> Option<PathBuf> {
        std::thread::spawn(move || unsafe { show(parent) })
            .join()
            .ok()
            .flatten()
    }

    /// SAFETY: every COM call below runs on this thread's own apartment, which
    /// is initialised on entry and uninitialised on every exit path. The one
    /// returned string is freed with the allocator the shell documents.
    unsafe fn show(parent: Option<isize>) -> Option<PathBuf> {
        // `COINIT_DISABLE_OLE1DDE` is what the shell dialog documentation asks
        // for; OLE1 DDE support is legacy and only slows initialisation.
        if CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_err() {
            return None;
        }

        let picked = pick(parent);

        CoUninitialize();
        picked
    }

    /// SAFETY: called only from `show`, inside an initialised apartment.
    unsafe fn pick(parent: Option<isize>) -> Option<PathBuf> {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;

        // Read the defaults and add to them rather than replacing: the shell
        // sets flags here that make the dialog behave like the rest of Windows.
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(
                options
                    // Directories only. Without this the same dialog picks
                    // files, which is not something this app has any use for.
                    | FOS_PICKFOLDERS
                    // A real place on a disk. Excludes virtual shell locations
                    // like "This PC" or a library, which have no path to scan.
                    | FOS_FORCEFILESYSTEM
                    // Refuse a folder that is not there any more.
                    | FOS_PATHMUSTEXIST,
            )
            .ok()?;

        dialog.SetTitle(w!("Choose a folder to protect")).ok()?;
        dialog.SetOkButtonLabel(w!("Protect this folder")).ok()?;

        // Cancelling returns an error here. That is the expected way out of a
        // dialog, so it is reported as "no folder", never as a failure.
        dialog
            .Show(parent.map(|handle| HWND(handle as *mut _)))
            .ok()?;

        let item = dialog.GetResult().ok()?;
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = raw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(raw.0 as *const _));

        path
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    pub fn pick_folder(_parent: Option<isize>) -> Option<PathBuf> {
        None
    }
}
