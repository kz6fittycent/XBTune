// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime environment: snap confinement and storage locations.
//!
//! Two kinds of storage:
//! - The user's files (tunes read from the ECM, backups, data logs, burn
//!   history) go in a folder the user can see: `~/XBTune` by default
//!   (the real home, not the snap's), or wherever they choose in the app's
//!   Settings (saved in `<config>/folders.toml`, read by the CLI too).
//! - XBTune's own files (settings, unlocks, the app log, unsent reports)
//!   stay out of the way: `$SNAP_USER_COMMON/{config,data}` in the snap,
//!   which survives refreshes and reverts (`$SNAP_USER_DATA` is per
//!   revision and never used), else the XDG directories.

use std::path::{Path, PathBuf};
use std::process::Command;

/// True when running inside a snap.
pub fn in_snap() -> bool {
    std::env::var_os("SNAP").is_some() && std::env::var_os("SNAP_NAME").is_some()
}

/// Name of the snap, when running inside one.
pub fn snap_name() -> Option<String> {
    std::env::var("SNAP_INSTANCE_NAME").or_else(|_| std::env::var("SNAP_NAME")).ok()
}

/// Folders inside the files folder.
pub const TUNES: &str = "Tunes";
pub const BACKUPS: &str = "Backups";
pub const LOGS: &str = "Logs";
pub const HISTORY: &str = "History";

/// Where XBTune keeps things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// XBTune's settings (settings.toml, hardware.toml, folders.toml).
    pub config: PathBuf,
    /// XBTune's own data (app log, unsent reports).
    pub data: PathBuf,
    /// The user's files: Tunes, Backups, Logs, History.
    pub files: PathBuf,
}

impl Dirs {
    /// `$XBTUNE_HOME/{config,data,files}` if set (tests and UI checks keep
    /// out of the user's folders), else `$SNAP_USER_COMMON/{config,data}`
    /// in the snap, else XDG directories; `files` as chosen in Settings,
    /// else `~/XBTune`.
    pub fn detect() -> Option<Self> {
        let (config, data, default_files) = if let Some(home) = std::env::var_os("XBTUNE_HOME") {
            let home = PathBuf::from(home);
            (home.join("config"), home.join("data"), home.join("files"))
        } else if in_snap()
            && let Some(common) = std::env::var_os("SNAP_USER_COMMON")
        {
            let common = PathBuf::from(common);
            (common.join("config"), common.join("data"), real_home()?.join("XBTune"))
        } else {
            let d = directories::ProjectDirs::from("", "", "xbtune")?;
            (d.config_dir().to_path_buf(), d.data_dir().to_path_buf(), real_home()?.join("XBTune"))
        };
        let files = chosen_files_dir(&config).unwrap_or(default_files);
        Some(Self { config, data, files })
    }

    /// Tunes read from the ECM, and the default place to save tunes.
    pub fn tunes(&self) -> PathBuf {
        self.files.join(TUNES)
    }

    /// EEPROM backups taken before writes.
    pub fn backups(&self) -> PathBuf {
        self.files.join(BACKUPS)
    }

    /// Data logs (CSV).
    pub fn logs(&self) -> PathBuf {
        self.files.join(LOGS)
    }

    /// One folder per burn: before.xbt, after.xbt, burn.txt.
    pub fn history(&self) -> PathBuf {
        self.files.join(HISTORY)
    }

    /// The files folder used when none is chosen.
    pub fn default_files(&self) -> Option<PathBuf> {
        match std::env::var_os("XBTUNE_HOME") {
            Some(home) => Some(PathBuf::from(home).join("files")),
            None => real_home().map(|h| h.join("XBTune")),
        }
    }
}

/// The user's real home folder (in the snap, `$HOME` is the snap's own).
pub fn real_home() -> Option<PathBuf> {
    std::env::var_os("SNAP_REAL_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()))
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
struct Folders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<PathBuf>,
}

fn folders_file(config: &Path) -> PathBuf {
    config.join("folders.toml")
}

fn chosen_files_dir(config: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(folders_file(config)).ok()?;
    toml::from_str::<Folders>(&text).ok()?.files.filter(|p| p.is_absolute())
}

/// Save the files folder (`None`: back to the default). Checks first that
/// the folder can be created and written to, which the snap's confinement
/// refuses for hidden folders in home and for most places outside it.
pub fn set_files_dir(config: &Path, dir: Option<&Path>) -> std::io::Result<()> {
    if let Some(d) = dir {
        if !d.is_absolute() {
            return Err(std::io::Error::other("the folder must be a full path"));
        }
        check_writable(d)?;
    }
    std::fs::create_dir_all(config)?;
    let text =
        toml::to_string(&Folders { files: dir.map(Path::to_path_buf) }).map_err(std::io::Error::other)?;
    let path = folders_file(config);
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, format!("# Where XBTune keeps tunes, backups, logs and burn history.\n{text}"))?;
    std::fs::rename(&tmp, &path)
}

/// Create `dir` if needed and prove a file can be written there.
pub fn check_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".xbtune-write-test");
    std::fs::write(&probe, b"ok")?;
    std::fs::remove_file(&probe)
}

/// What [`move_files`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    pub files: usize,
    /// Files left where they were, and why.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Move everything in each `(from, to)` folder into `to`, keeping the
/// layout. Never overwrites: a name already taken in `to` is left in
/// `from` and listed. Across filesystems a file is copied, checked for
/// size, then removed. Emptied folders are removed.
pub fn move_files(pairs: &[(PathBuf, PathBuf)]) -> Moved {
    let mut moved = Moved::default();
    for (from, to) in pairs {
        if from == to || !from.is_dir() {
            continue;
        }
        if to.starts_with(from) {
            moved.skipped.push((from.clone(), "the new folder is inside it".into()));
            continue;
        }
        move_tree(from, to, &mut moved);
        remove_empty_dirs(from);
    }
    moved
}

fn move_tree(from: &Path, to: &Path, moved: &mut Moved) {
    let Ok(entries) = std::fs::read_dir(from) else { return };
    if let Err(e) = std::fs::create_dir_all(to) {
        moved.skipped.push((from.to_path_buf(), e.to_string()));
        return;
    }
    for entry in entries.flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            move_tree(&src, &dst, moved);
            continue;
        }
        if dst.exists() {
            moved.skipped.push((src, format!("{} already exists", dst.display())));
            continue;
        }
        match move_file(&src, &dst) {
            Ok(()) => moved.files += 1,
            Err(e) => moved.skipped.push((src, e.to_string())),
        }
    }
}

fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    let n = std::fs::copy(src, dst)?;
    if n != std::fs::metadata(src)?.len() {
        let _ = std::fs::remove_file(dst);
        return Err(std::io::Error::other("copy came out a different size"));
    }
    std::fs::remove_file(src)
}

fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                remove_empty_dirs(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir); // only succeeds when empty
}

/// The four subfolders of one files folder paired with another's.
pub fn folder_pairs(from_files: &Path, to_files: &Path) -> Vec<(PathBuf, PathBuf)> {
    [TUNES, BACKUPS, LOGS, HISTORY].iter().map(|n| (from_files.join(n), to_files.join(n))).collect()
}

/// Before 2026-10-15 the user's files lived in `<data>/{images,backups,
/// logs,history}`. Move them into the files folder (once; later runs find
/// nothing to move).
pub fn move_old_layout(d: &Dirs) -> Moved {
    move_files(&[
        (d.data.join("images"), d.tunes()),
        (d.data.join("backups"), d.backups()),
        (d.data.join("logs"), d.logs()),
        (d.data.join("history"), d.history()),
    ])
}

/// Snap plugs XBTune needs that are not auto-connected.
pub const MANUAL_PLUGS: &[(&str, &str)] = &[
    ("bluez", "Bluetooth adapters (BuellTooth)"),
    ("raw-usb", "USB serial adapters (/dev/ttyUSB*, /dev/ttyACM*)"),
    ("removable-media", "EEPROM images and logs on USB sticks"),
];

/// Connection state of one plug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlugState {
    pub plug: &'static str,
    pub purpose: &'static str,
    /// `None` if it could not be determined.
    pub connected: Option<bool>,
}

impl PlugState {
    /// The command that connects this plug.
    pub fn connect_command(&self) -> String {
        format!("sudo snap connect {}:{}", snap_name().unwrap_or_else(|| "xbtune".into()), self.plug)
    }
}

/// Ask snapd which of [`MANUAL_PLUGS`] are connected. Returns an empty list
/// outside a snap.
pub fn plug_states() -> Vec<PlugState> {
    if !in_snap() {
        return Vec::new();
    }
    MANUAL_PLUGS
        .iter()
        .map(|&(plug, purpose)| {
            // `snapctl is-connected` exits 0 if connected, 1 if not.
            let connected =
                Command::new("snapctl").args(["is-connected", plug]).status().ok().and_then(|s| {
                    match s.code() {
                        Some(0) => Some(true),
                        Some(1) => Some(false),
                        _ => None,
                    }
                });
            PlugState { plug, purpose, connected }
        })
        .collect()
}

/// Advice for a "permission denied" opening a transport.
pub fn permission_hint(plug: &str) -> String {
    let connected = plug_states().into_iter().find(|p| p.plug == plug).and_then(|p| p.connected);
    if in_snap() && connected == Some(true) {
        format!(
            "The '{plug}' interface is connected, but confinement still refused this operation.\n\
             This is an XBTune bug. Please report it with the output of:\n    \
             sudo journalctl -k --since -5min | grep xbtune"
        )
    } else if in_snap() {
        format!(
            "The snap is not allowed to use this device. Run:\n    sudo snap connect {}:{plug}",
            snap_name().unwrap_or_else(|| "xbtune".into())
        )
    } else if plug == "raw-usb" {
        "Your user cannot open the serial port. Add yourself to the 'dialout' group:\n    \
         sudo usermod -aG dialout $USER   (then log out and back in)"
            .to_string()
    } else {
        "Bluetooth access was refused. Check that Bluetooth is on and not blocked (rfkill list).".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xbtune-env-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn old_layout_moves_once_and_never_overwrites() {
        let root = scratch("migrate");
        let d = Dirs { config: root.join("config"), data: root.join("data"), files: root.join("XBTune") };
        std::fs::create_dir_all(d.data.join("images")).unwrap();
        std::fs::create_dir_all(d.data.join("history/BUEIB_1")).unwrap();
        std::fs::create_dir_all(d.data.join("backups")).unwrap();
        std::fs::create_dir_all(d.data.join("app-log")).unwrap();
        std::fs::write(d.data.join("images/a.xbt"), "a").unwrap();
        std::fs::write(d.data.join("history/BUEIB_1/burn.txt"), "h").unwrap();
        std::fs::write(d.data.join("backups/b.xbt"), "old b").unwrap();
        std::fs::write(d.data.join("app-log/app.log"), "log").unwrap();
        // A file of the same name already in the new place is kept.
        std::fs::create_dir_all(d.backups()).unwrap();
        std::fs::write(d.backups().join("b.xbt"), "new b").unwrap();

        let m = move_old_layout(&d);
        assert_eq!(m.files, 2, "{m:?}");
        assert_eq!(m.skipped.len(), 1);
        assert_eq!(std::fs::read_to_string(d.tunes().join("a.xbt")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(d.history().join("BUEIB_1/burn.txt")).unwrap(), "h");
        assert_eq!(std::fs::read_to_string(d.backups().join("b.xbt")).unwrap(), "new b");
        assert_eq!(std::fs::read_to_string(d.data.join("backups/b.xbt")).unwrap(), "old b");
        // Emptied folders go; XBTune's own files stay.
        assert!(!d.data.join("images").exists() && !d.data.join("history").exists());
        assert!(d.data.join("app-log/app.log").exists());
        // Nothing left to move the second time.
        assert_eq!(move_old_layout(&d).files, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn moving_into_a_subfolder_of_itself_is_refused() {
        let root = scratch("inside");
        std::fs::create_dir_all(root.join("Tunes")).unwrap();
        std::fs::write(root.join("Tunes/t.xbt"), "t").unwrap();
        let m = move_files(&[(root.join("Tunes"), root.join("Tunes/deeper"))]);
        assert_eq!(m.files, 0);
        assert_eq!(m.skipped.len(), 1);
        assert!(root.join("Tunes/t.xbt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn chosen_folder_round_trips() {
        let root = scratch("chosen");
        let config = root.join("config");
        assert_eq!(chosen_files_dir(&config), None);
        let mine = root.join("My Bike Files");
        set_files_dir(&config, Some(&mine)).unwrap();
        assert_eq!(chosen_files_dir(&config), Some(mine.clone()));
        assert!(mine.is_dir(), "created and checked writable");
        set_files_dir(&config, None).unwrap();
        assert_eq!(chosen_files_dir(&config), None);
        assert!(set_files_dir(&config, Some(Path::new("relative/dir"))).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
