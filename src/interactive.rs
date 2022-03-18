//       ___           ___           ___           ___
//      /\__\         /\  \         /\  \         /\__\
//     /:/  /         \:\  \        \:\  \       /::|  |
//    /:/__/           \:\  \        \:\  \     /:|:|  |
//   /::\  \ ___       /::\  \       /::\  \   /:/|:|__|__
//  /:/\:\  /\__\     /:/\:\__\     /:/\:\__\ /:/ |::::\__\
//  \/__\:\/:/  /    /:/  \/__/    /:/  \/__/ \/__/~~/:/  /
//       \::/  /    /:/  /        /:/  /            /:/  /
//       /:/  /     \/__/         \/__/            /:/  /
//      /:/  /                                    /:/  /
//      \/__/                                     \/__/
//
// (c) Robert Swinford <robert.swinford<...at...>gmail.com>
//
// For the full copyright and license information, please view the LICENSE file
// that was distributed with this source code.

use crate::display::{display_exec, paint_string};
use crate::lookup::lookup_exec;
use crate::{get_pathdata, read_stdin};
use crate::{Config, HttmError, InteractiveMode};

extern crate skim;
use chrono::DateTime;
use chrono::Local;
use skim::prelude::*;
use skim::DisplayContext;
use std::{
    ffi::OsStr,
    fs::ReadDir,
    io::{Cursor, Stdout, Write as IoWrite},
    path::PathBuf,
    process::Command as ExecProcess,
    thread,
    time::SystemTime,
    vec,
};

struct SelectionCandidate {
    path: PathBuf,
}

impl SkimItem for SelectionCandidate {
    fn text(&self) -> Cow<str> {
        self.path
            .file_name()
            .unwrap_or_else(|| OsStr::new(""))
            .to_string_lossy()
    }
    fn display<'a>(&'a self, _context: DisplayContext<'a>) -> AnsiString<'a> {
        AnsiString::parse(&paint_string(
            &self.path,
            &self
                .path
                .file_name()
                .unwrap_or_else(|| OsStr::new(""))
                .to_string_lossy(),
        ))
    }
    fn output(&self) -> Cow<str> {
        let path = self
            .path
            .canonicalize()
            .unwrap_or_else(|_| self.path.clone())
            .to_string_lossy()
            .into_owned();
        Cow::Owned(path)
    }
}

pub fn interactive_exec(
    out: &mut Stdout,
    config: &Config,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    // are the raw paths as strings suitable for interactive mode
    let paths_as_strings = vec![lookup_view(config)?];

    // do we return back to our exec function to print or into interactive mode?
    match config.interactive_mode {
        InteractiveMode::Restore | InteractiveMode::Select => {
            interactive_select(out, config, paths_as_strings)?;
            unreachable!()
        }
        // InteractiveMode::Lookup executes back through fn exec() in httm.rs
        _ => Ok(paths_as_strings),
    }
}

fn interactive_select(
    out: &mut Stdout,
    config: &Config,
    paths_as_strings: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // same stuff we do at exec, snooze...
    let search_path = paths_as_strings.get(0).unwrap().to_owned();
    let pathdata_set = get_pathdata(config, &[search_path])?;
    let snaps_and_live_set = lookup_exec(config, pathdata_set)?;
    let selection_buffer = display_exec(config, snaps_and_live_set)?;

    // get file name, and get ready to do some file ops!!
    // ... we want the 2nd item or the indexed "1" object
    // everything between the quotes
    let requested_file_name = select_view(selection_buffer)?;
    let broken_string: Vec<_> = requested_file_name.split_terminator('"').collect();
    let parsed_str = if let Some(parsed) = broken_string.get(1) {
        parsed
    } else {
        return Err(HttmError::new("Invalid value selected. Quitting.").into());
    };

    if config.interactive_mode == InteractiveMode::Restore {
        Ok(interactive_restore(out, config, parsed_str)?)
    } else {
        writeln!(out, "\"{}\"", parsed_str)?;
        std::process::exit(0)
    }
}

fn interactive_restore(
    out: &mut Stdout,
    config: &Config,
    parsed_str: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let snap_pbuf = PathBuf::from(&parsed_str);

    let snap_md = if let Ok(snap_md) = snap_pbuf.metadata() {
        snap_md
    } else {
        return Err(HttmError::new("Snapshot location does not exist on disk. Quitting.").into());
    };

    // build new place to send file
    let old_snap_filename = snap_pbuf
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let new_snap_filename: String =
        old_snap_filename.clone() + ".httm_restored." + &timestamp_file(&snap_md.modified()?);

    let new_file_dir = config.current_working_dir.clone();
    let new_file_pbuf: PathBuf = [new_file_dir, PathBuf::from(new_snap_filename)]
        .iter()
        .collect();

    let old_file_dir = config.current_working_dir.clone();
    let old_file_pbuf: PathBuf = [old_file_dir, PathBuf::from(old_snap_filename)]
        .iter()
        .collect();

    if old_file_pbuf == snap_pbuf {
        return Err(
            HttmError::new("Will not restore files as files are the same file. Quitting.").into(),
        );
    };

    // tell the user what we're up to
    write!(out, "httm will copy a file from a ZFS snapshot...\n\n")?;
    writeln!(out, "\tfrom: {:?}", snap_pbuf)?;
    writeln!(out, "\tto:   {:?}\n", new_file_pbuf)?;
    write!(
        out,
        "Before httm does anything, it would like your consent. Continue? (Y/N) "
    )?;
    out.flush()?;

    let input_buffer = read_stdin()?;
    let res = input_buffer
        .get(0)
        .unwrap_or(&"N".to_owned())
        .to_lowercase();

    if res == "y" || res == "yes" {
        std::fs::copy(snap_pbuf, new_file_pbuf)?;
        write!(out, "\nRestore completed successfully.\n")?;
    } else {
        write!(out, "\nUser declined.  No files were restored.\n")?;
    }

    std::process::exit(0)
}

fn lookup_view(config: &Config) -> Result<String, Box<dyn std::error::Error>> {
    // We can build a method on our SkimItem to do this, except, right now, it's slower
    // because it blocks on preview(), given the implementation of skim, see the new_preview branch

    // prep thread spawn
    let mut read_dir = std::fs::read_dir(&config.user_requested_dir)?;
    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    let config_clone = config.clone();

    // spawn fn enumerate_directory - useful for recursive mode
    thread::spawn(move || {
        enumerate_directory(&config_clone, &tx_item, &mut read_dir);
    });

    // as skim is slower if we use a function, we must call a command
    // and that cause all sorts of nastiness with PATHs etc if the user
    // is not expecting it, so we must locate which command to use.

    let httm_pwd_cmd: PathBuf = [&config.current_working_dir, &PathBuf::from("httm")]
        .iter()
        .collect();
    let httm_path_cmd =
        std::str::from_utf8(&ExecProcess::new("which").arg("httm").output()?.stdout)?.to_owned();

    // string to exec on each preview
    let httm_command = if httm_pwd_cmd.exists() {
        httm_pwd_cmd.to_string_lossy().into_owned()
    } else if !httm_path_cmd.is_empty() {
        httm_path_cmd.trim_end_matches('\n').to_owned()
    } else {
        return Err(HttmError::new(
            "You must place the 'httm' command in your path.  Perhaps the .cargo/bin folder isn't in your path?",
        )
        .into());
    };

    // create command to use for preview, as noted, unable to use a function for now
    let preview_str = if let Some(raw_value) = &config.opt_snap_point {
        let snap_point = raw_value.to_string_lossy();
        let local_dir = &config.opt_local_dir.to_string_lossy();
        format!("\"{httm_command}\" --snap-point \"{snap_point}\" --local-dir \"{local_dir}\" {{}}")
    } else {
        format!("\"{httm_command}\" {{}}")
    };

    // create the skim component for previews
    let options = SkimOptionsBuilder::default()
        .preview_window(Some("70%"))
        .preview(Some(&preview_str))
        .build()
        .unwrap();

    let selected_items = Skim::run_with(&options, Some(rx_item))
        .map(|out| out.selected_items)
        .unwrap_or_else(Vec::new);

    let res = selected_items
        .iter()
        .map(|i| i.output().into_owned())
        .collect();

    Ok(res)
}

fn select_view(selection_buffer: String) -> Result<String, Box<dyn std::error::Error>> {
    let options = SkimOptionsBuilder::default()
        .interactive(true)
        .build()
        .unwrap();

    // `SkimItemReader` is a helper to turn any `BufRead` into a stream of `SkimItem`
    // `SkimItem` was implemented for `AsRef<str>` by default
    let item_reader = SkimItemReader::default();
    let items = item_reader.of_bufread(Cursor::new(selection_buffer));

    // `run_with` would read and show items from the stream
    let selected_items = Skim::run_with(&options, Some(items))
        .map(|out| out.selected_items)
        .unwrap_or_else(Vec::new);

    let res = selected_items
        .iter()
        .map(|i| i.output().into_owned())
        .collect();

    Ok(res)
}

fn enumerate_directory(config: &Config, tx_item: &SkimItemSender, read_dir: &mut ReadDir) {
    // convert to paths
    let (vec_files, vec_dirs): (Vec<PathBuf>, Vec<PathBuf>) = read_dir
        .filter_map(|i| i.ok())
        .map(|dir_entry| dir_entry.path())
        .partition(|path| path.is_file() || path.is_symlink());

    // display with pretty ANSI colors
    let mut combined_vec: Vec<&PathBuf> =
        vec![&vec_files, &vec_dirs].into_iter().flatten().collect();
    combined_vec.sort();
    combined_vec.iter().for_each(|path| {
        let _ = tx_item.send(Arc::new(SelectionCandidate {
            path: path.to_path_buf(),
        }));
    });

    // now recurse into dirs, if requested
    if config.opt_recursive {
        vec_dirs
            .iter()
            .filter_map(|read_dir| std::fs::read_dir(read_dir).ok())
            .for_each(|mut read_dir| {
                enumerate_directory(config, tx_item, &mut read_dir);
            })
    }
}

fn timestamp_file(st: &SystemTime) -> String {
    let dt: DateTime<Local> = st.to_owned().into();
    format!("{}", dt.format("%b-%d-%H:%M:%S-%Y"))
}
