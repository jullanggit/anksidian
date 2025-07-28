#![feature(exit_status_error)]
#![feature(array_windows)]
#![feature(string_into_chars)]
#![feature(iter_intersperse)]
#![feature(iter_map_windows)]
#![feature(file_buffered)]
#![feature(associated_type_defaults)]

use blake3::{Hash, Hasher};
use log::trace;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env::{self, VarError},
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read},
    ops::Not,
    path::{Path, PathBuf},
    sync::LazyLock,
};
use thiserror::Error;

use crate::{
    anki::{handle_unseen_notes, initialize_notes},
    handle_md::{HandleMdError, handle_md},
};

mod anki;
mod handle_md;

static DECK: LazyLock<String> = LazyLock::new(|| {
    let deck_name = env::args()
        .nth(1)
        .expect("The deck name should be passed as the first argument");
    assert_ne!(
        &deck_name[0..2],
        "--",
        "Deck name shouldn't start with '--'"
    );
    deck_name
});
static CLIENT: LazyLock<Client> = LazyLock::new(reqwest::Client::new);

const IGNORE_PATHS: [&str; 1] = ["./Excalidraw"];

#[tokio::main]
async fn main() {
    env_logger::init();

    initialize_notes()
        .await
        .expect("Failed to initialize notes");

    let no_cache = env::args().skip(2).any(|arg| &arg == "--no-cache");
    let mut file_cache = no_cache
        .not()
        .then(|| FileCache::load().expect("Failed to load file cache"));

    traverse(PathBuf::from("."), &mut file_cache)
        .await
        .expect("Failed to traverse");

    if let Some(file_cache) = file_cache {
        file_cache.save().expect("Failed to save file cache");
    // only handle unseen notes if we dont use a cache, as we otherwise get false positives
    } else {
        handle_unseen_notes()
            .await
            .expect("Failed to handle unseen notes");
    };
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
    /// deck -> file -> hash
    hashes: HashMap<String, HashMap<PathBuf, Hash>>,
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
    #[error("Failed to hash file '{file}': {error}")]
    Hash {
        error: std::io::Error,
        file: PathBuf,
    },
}
async fn traverse(dir: PathBuf, file_cache: &mut Option<FileCache>) -> Result<(), TraverseError> {
    trace!("Recursing into dir {}", dir.display());
    for entry in dir
        .read_dir()
        .map_err(|error| TraverseError::ReadDir { error, dir })?
        .flatten()
    {
        let path = entry.path();
        // recurse
        if path.is_dir()
            && !IGNORE_PATHS
                .map(AsRef::<Path>::as_ref)
                .contains(&path.as_path())
        {
            Box::pin(traverse(path, file_cache)).await?;
        // markdown file
        } else if path.is_file()
            && let Some(extension) = path.extension()
            && extension == "md"
        {
            let handle_and_wrap_md = async |path: &Path| {
                handle_md(path)
                    .await
                    .map_err(|error| TraverseError::HandleMd {
                        error,
                        file: path.to_path_buf(),
                    })
            };
            match file_cache {
                None => handle_and_wrap_md(&path).await?,
                Some(file_cache) => {
                    let file_hash = hash_file(&path).map_err(|error| TraverseError::Hash {
                        error,
                        file: path.clone(),
                    })?;
                    match file_cache.hashes.get_mut(&*DECK) {
                        // deck is in cache
                        Some(deck_cache) => {
                            // file isn't in cache or hashes don't match
                            if deck_cache.get(&path) != Some(&file_hash) {
                                handle_and_wrap_md(&path).await?;
                                deck_cache.insert(path, file_hash);
                            }
                        }
                        // deck is not in cache
                        None => {
                            handle_and_wrap_md(&path).await?;
                            file_cache
                                .hashes
                                .insert(DECK.clone(), HashMap::from([(path, file_hash)]));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
