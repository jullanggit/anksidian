#![feature(exit_status_error)]
#![feature(array_windows)]
#![feature(string_into_chars)]
#![feature(iter_intersperse)]
#![feature(iter_map_windows)]
#![feature(file_buffered)]
#![feature(associated_type_defaults)]

use blake3::{Hash, Hasher};
use log::trace;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env::{self, VarError, home_dir},
    fmt::Display,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read},
    ops::Not,
    path::{Path, PathBuf},
    process::exit,
    sync::LazyLock,
};
use thiserror::Error;
use ureq::Agent;

use crate::{
    anki::{handle_unseen_notes, initialize_notes},
    handle_md::{HandleMdError, MarkNotesAsSeenError, handle_md, mark_notes_as_seen},
};

mod anki;
mod handle_md;

#[derive(Deserialize, Serialize, Clone)]
struct Config {
    path_to_deck: Vec<PathToDeck>,
    #[serde(with = "serde_regex")]
    ignore_paths: Vec<Regex>,
    disable_typst: bool,
}
impl Default for Config {
    fn default() -> Self {
        Config {
            path_to_deck: vec![PathToDeck {
                path: Regex::new(".*").expect("Should be a valid regex"),
                deck: "Obsidian".to_string(),
            }],
            ignore_paths: vec![Regex::new(".*Excalidraw").expect("Should be a valid regex")],
            disable_typst: false,
        }
    }
}
#[test]
fn test_default_config() {
    Config::default();
}

#[derive(Deserialize, Serialize, Clone)]
struct PathToDeck {
    #[serde(with = "serde_regex")]
    path: Regex,
    deck: String,
}

static CONFIG: LazyLock<Config> = LazyLock::new(|| {
    let path = home_dir()
        .expect("Failed to get home directory")
        .join(".config/anksidian/config.json");

    let config = if !fs::exists(&path).expect("Failed to check if folder to deck config exists") {
        if let Err(err) = fs::create_dir_all(
            path.parent()
                .expect("Path always has a parent, as we join a multi-part path onto it."),
        ) {
            match err.kind() {
                io::ErrorKind::AlreadyExists => {}
                other => panic!("Failed to create parent dirs for folder to deck config: {other}"),
            }
        }

        let default = Config::default();

        let json = serde_json::to_string_pretty(&default)
            .expect("Failed to serialize default folder to deck config");
        fs::write(path, json).expect("Failed to write default folder to deck config");

        default
    } else {
        let string = fs::read_to_string(path).expect("Failed to read folder to deck config");
        serde_json::from_str(&string).expect("Failed to deserialize folder to deck config")
    };

    // ensure all decks mentioned in config exist
    for PathToDeck { deck, .. } in &config.path_to_deck {
        anki::ensure_deck_exists(deck).expect("Failed to ensure that deck exists")
    }

    config
});
static AGENT: LazyLock<Agent> = LazyLock::new(Agent::new_with_defaults);
static PWD: LazyLock<PathBuf> =
    LazyLock::new(|| env::current_dir().expect("Failed to get current working directory"));

/// Unwraps the result, display-printing and exiting the program on errors.
fn exit_on_err<T, E: Display>(res: Result<T, E>, msg: &str) -> T {
    match res {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{msg}: {e}");
            exit(1);
        }
    }
}

fn main() {
    env_logger::init();

    exit_on_err(initialize_notes(), "Failed to initialize notes");

    let track_seen = env::args().skip(2).any(|arg| &arg == "--track-seen");
    let mut file_cache = env::args()
        .skip(2)
        .any(|arg| &arg == "--no-cache")
        .not()
        .then(|| match FileCache::load() {
            Ok(cache) => Some(cache),
            Err(error) => {
                log::error!("Failed to load file cache, continuing without it: {error}");
                None
            }
        })
        .flatten();

    exit_on_err(
        traverse(PathBuf::from("."), &mut file_cache, track_seen),
        "Failed to traverse directory",
    );

    // handle unseen notes if we have seen all present notes
    if (file_cache.is_none() || track_seen)
        && let Err(err) = handle_unseen_notes()
    {
        log::error!("Failed to handle unseen notes: {err}");
    };

    // save file cache
    if let Some(file_cache) = file_cache
        && let Err(error) = file_cache.save()
    {
        log::error!("Failed to save file cache: {error}")
    }
}

#[derive(Error, Debug)]
enum FileCacheLoadError {
    #[error("Failed to get path to file cache: {0}")]
    GetPath(#[from] VarError),
    #[error("Failed to open file cache: {0}")]
    Open(#[from] std::io::Error),
    #[error("Failed to deserialize file cache: {0}")]
    Deserialize(#[from] serde_json::Error),
}

#[derive(Error, Debug)]
enum FileCacheSaveError {
    #[error("Failed to get path to file cache: {0}")]
    GetPath(#[from] VarError),
    #[error("Failed to create parent paths for the file cache: {0}")]
    CreateParents(std::io::Error),
    #[error("Failed to open file cache: {0}")]
    Open(std::io::Error),
    #[error("Failed to serialize file cache: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Serialize, Deserialize, Default)]
struct FileCache {
    /// source_dir -> file -> hash
    hashes: HashMap<PathBuf, HashMap<PathBuf, Hash>>,
}
impl FileCache {
    fn get_path() -> Result<PathBuf, VarError> {
        let mut cache = PathBuf::from(env::var("XDG_CACHE_HOME").or_else(|_| {
            env::var("HOME").map(|mut home| {
                home.push_str("/.cache");
                home
            })
        })?);
        cache.push("anksidian");
        cache.push("file_cache.json");

        Ok(cache)
    }
    fn load() -> Result<Self, FileCacheLoadError> {
        let path = Self::get_path()?;
        if !path.exists() {
            Ok(Self::default())
        } else {
            let file = File::open_buffered(&path).map_err(FileCacheLoadError::Open)?;
            Ok(serde_json::from_reader(file)?)
        }
    }
    fn save(&self) -> Result<(), FileCacheSaveError> {
        let path = Self::get_path()?;
        let parent = path.parent().expect("Path should have a parent");
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(FileCacheSaveError::CreateParents)?;
        }
        let file = BufWriter::new(
            OpenOptions::new()
                .read(false)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .map_err(FileCacheSaveError::Open)?,
        );
        serde_json::to_writer(file, self)?;
        Ok(())
    }
}
fn hash_file(path: &Path) -> std::io::Result<Hash> {
    let mut file = File::open_buffered(path)?;
    let mut hasher = Hasher::new();

    let mut buffer = [0; 4096];
    loop {
        let bytes_read = file.read(buffer.as_mut_slice())?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(hasher.finalize())
}

#[derive(Error, Debug)]
enum TraverseError {
    #[error("Failed to read dir '{dir}': {error}")]
    ReadDir { error: std::io::Error, dir: PathBuf },
    #[error("Failed to handle md file '{file}': {error}")]
    HandleMd { error: HandleMdError, file: PathBuf },
    #[error("Failed to mark notes as seen if file '{file}': {error}")]
    MarkNotesAsSeen {
        error: MarkNotesAsSeenError,
        file: PathBuf,
    },
    #[error("Failed to hash file '{file}': {error}")]
    Hash {
        error: std::io::Error,
        file: PathBuf,
    },
    #[error("Failed to canonicalize (expand) path {path}: {error}")]
    CanonicalizePath { path: PathBuf, error: io::Error },
}
fn traverse(
    dir: PathBuf,
    file_cache: &mut Option<FileCache>,
    track_seen: bool,
) -> Result<(), TraverseError> {
    trace!("Recursing into dir {}", dir.display());
    for entry in dir
        .read_dir()
        .map_err(|error| TraverseError::ReadDir { error, dir })?
        .flatten()
    {
        let path = entry.path();
        let canonicalized =
            path.canonicalize()
                .map_err(|error| TraverseError::CanonicalizePath {
                    path: path.to_path_buf(),
                    error,
                })?;
        // recurse
        if path.is_dir()
            && !CONFIG
                .ignore_paths
                .iter()
                .any(|ignore_path| ignore_path.is_match(&canonicalized.to_string_lossy()))
        {
            traverse(path, file_cache, track_seen)?;
        // markdown file
        } else if path.is_file()
            && let Some(extension) = path.extension()
            && extension == "md"
        {
            let handle_and_wrap_md = |path: &Path| {
                handle_md(path).map_err(|error| TraverseError::HandleMd {
                    error,
                    file: path.to_path_buf(),
                })
            };
            match file_cache {
                None => handle_and_wrap_md(&path)?,
                Some(file_cache) => {
                    let file_hash = hash_file(&path).map_err(|error| TraverseError::Hash {
                        error,
                        file: path.clone(),
                    })?;
                    match file_cache.hashes.get_mut(&*PWD) {
                        // current dir is in cache
                        Some(deck_cache) => {
                            // file isn't in cache or hashes don't match
                            if deck_cache.get(&path) != Some(&file_hash) {
                                handle_and_wrap_md(&path)?;
                                deck_cache.insert(path, file_hash);
                            } else if track_seen {
                                mark_notes_as_seen(&path).map_err(|error| {
                                    TraverseError::MarkNotesAsSeen {
                                        error,
                                        file: path.clone(),
                                    }
                                })?;
                            }
                        }
                        // current_dir is not in cache
                        None => {
                            handle_and_wrap_md(&path)?;
                            file_cache
                                .hashes
                                .insert(PWD.clone(), HashMap::from([(path, file_hash)]));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
