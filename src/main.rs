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
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read},
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
    let mut file_cache = no_cache.not().then(FileCache::load);

    traverse(PathBuf::from("."), &mut file_cache)
        .await
        .expect("Failed to traverse");

    if let Some(file_cache) = file_cache {
        file_cache.save()
    // only handle unseen notes if we dont use a cache, as we otherwise get false positives
    } else {
        handle_unseen_notes()
            .await
            .expect("Failed to handle unseen notes");
    };
}

#[derive(Serialize, Deserialize, Default)]
struct FileCache {
    /// deck -> file -> hash
    hashes: HashMap<String, HashMap<PathBuf, Hash>>,
}
impl FileCache {
    fn get_path() -> PathBuf {
        let mut cache = PathBuf::from(
            env::var("XDG_CACHE_HOME")
                .or_else(|_| {
                    env::var("HOME").map(|mut home| {
                        home.push_str("/.cache");
                        home
                    })
                })
                .expect("Failed to get cache directory"),
        );
        cache.push("anksidian");
        cache.push("file_cache.json");

        cache
    }
    fn load() -> Self {
        let path = Self::get_path();
        if !path.exists() {
            Self::default()
        } else {
            let file = File::open_buffered(&path).expect("Failed to open file cache");
            serde_json::from_reader(file).expect("Failed to deserialize file cache")
        }
    }
    fn save(&self) {
        let path = Self::get_path();
        let parent = path.parent().expect("Path should have a parent");
        if !parent.exists() {
            fs::create_dir_all(parent).expect("Failed to create parent path");
        }
        let file = BufWriter::new(
            OpenOptions::new()
                .read(false)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .expect("Failed to open file cache for saving"),
        );
        serde_json::to_writer(file, self).expect("Failed to serialize file cache");
    }
}
fn hash_file(path: &Path) -> Hash {
    let mut file = File::open_buffered(path).expect("Couldn't open file for hashing");
    let mut hasher = Hasher::new();

    let mut buffer = [0; 4096];
    loop {
        let bytes_read = file
            .read(buffer.as_mut_slice())
            .expect("Couldn't read file for hashing");
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    hasher.finalize()
}

#[derive(Error, Debug)]
enum TraverseError {
    #[error("{0}")]
    HandleMd(#[from] HandleMdError),
    // TODO: split up into more specific errors
    #[error("{0}")]
    Io(#[from] std::io::Error),
}
async fn traverse(dir: PathBuf, file_cache: &mut Option<FileCache>) -> Result<(), TraverseError> {
    trace!("Recursing into dir {}", dir.display());
    for entry in dir.read_dir()?.flatten() {
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
            match file_cache {
                None => handle_md(&path).await?,
                Some(file_cache) => {
                    let file_hash = hash_file(&path);
                    match file_cache.hashes.get_mut(&*DECK) {
                        // deck is in cache
                        Some(deck_cache) => {
                            // file isn't in cache or hashes don't match
                            if deck_cache.get(&path) != Some(&file_hash) {
                                handle_md(&path).await?;
                                deck_cache.insert(path, file_hash);
                            }
                        }
                        // deck is not in cache
                        None => {
                            handle_md(&path).await?;
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
