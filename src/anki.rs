use log::{debug, warn};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::HashMap,
    fmt::Debug,
    io::stdin,
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};
use thiserror::Error;
use tokio::time::sleep;

use crate::{CLIENT, DECK};

// Handles interaction with AnkiConnect.
// Could maybe use a bit more type-safety, stuff like action <-> params,
// and model <-> fields could be linked, but we dont really need it here and
// it would complicate the serialization

const MAX_BACKOFF: u8 = 5;

// UpdateNote, because it contains all information we need and can be converted to an AddNote with only defaultable values missing.
/// (note, seen)
pub static NOTES: Mutex<Vec<(UpdateNote, bool)>> = Mutex::new(Vec::new());

#[derive(Error, Debug)]
pub enum RequestError {
    #[error("AnkiConnect request failed: {0}")]
    AnkiConncectRequest(reqwest::Error),
    #[error("Failed to deserialize response: {0}")]
    Deserialisation(#[from] reqwest::Error),
    #[error("AnkiConnect returned error: {0}")]
    AnkiConnectError(String),
    // We would like to also include the value of the result here, but it would also need to implement Debug + Display etc. (which for example () doesn't)
    #[error("AnkiConnect returned both an error ({error}) and a result")]
    ErrorAndResult { error: String },
    #[error("AnkiConnect returned neither an error nor a result")]
    ErrorNorResult,
    #[error("AnkiConnect request returned an erroneous status code: {0}")]
    ErrStatus(StatusCode),
}

trait Request: Debug + Serialize {
    type Output: DeserializeOwned + Debug = ();
    fn action_type() -> ActionType;
    async fn request(&self) -> Result<Self::Output, RequestError> {
        #[derive(Serialize, Debug)]
        #[serde(rename_all = "camelCase")]
        struct Request<T> {
            action: ActionType,
            version: u8,
            params: T,
        }

        let request = CLIENT.post("http://localhost:8765").json(&Request {
            action: Self::action_type(),
            version: 6,
            params: self,
        });
        let mut i = 0;
        let response = loop {
            let timeout = Duration::from_millis(100 * 2_u64.pow(i.into()));
            match request
                .try_clone()
                .expect("request body should be cloneable, as it isn't a stream")
                .send()
                .await
            {
                Ok(response) => break response,
                Err(e) if i < MAX_BACKOFF => {
                    warn!("AnkiConnect request failed (attempt {i}): {e}. Retrying in {timeout:?}");
                    sleep(timeout).await;
                }
                Err(e) => Err(RequestError::AnkiConncectRequest(e))?,
            }
            i += 1;
        };

        let response = if response.status().is_success() {
            let response: Response<Self::Output> = response.json().await?;
            match (response.result, response.error) {
                (Some(result), None) => Ok(result),
                (None, Some(error)) => Err(RequestError::AnkiConnectError(error)),
                (Some(_), Some(error)) => Err(RequestError::ErrorAndResult { error }),
                (None, None) => Err(RequestError::ErrorNorResult),
            }
        } else {
            Err(RequestError::ErrStatus(response.status()))
        };
        debug!("Got response {response:?} from AnkiConnect for request {self:?}");
        response
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
enum ActionType {
    AddNote,
    DeleteNotes,
    UpdateNote,
    NotesInfo,
    CreateDeck,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Note<T> {
    note: T,
}
impl<T: Request> Request for Note<T> {
    type Output = T::Output;
    fn action_type() -> ActionType {
        T::action_type()
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNote {
    pub id: NoteId,
    pub fields: HashMap<String, String>,
    tags: Vec<String>,
}
impl Request for UpdateNote {
    fn action_type() -> ActionType {
        ActionType::UpdateNote
    }
}

#[derive(Deserialize, Debug)]
struct Response<T> {
    result: Option<T>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(transparent)]
/// Contains a Unix Timestamp (so 13 decimal digits for the years 2001-2286)
pub struct NoteId(pub u64);

pub type LockNotesError = PoisonError<MutexGuard<'static, Vec<(UpdateNote, bool)>>>;

#[derive(Error, Debug)]
pub enum InitializeNotesError {
    #[error("Failed to request notes: {0}")]
    Request(#[from] RequestError),
    #[error("Failed to lock NOTES: {0}")]
    Lock(#[from] LockNotesError),
}
pub async fn initialize_notes() -> Result<(), InitializeNotesError> {
    #[derive(Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    struct Field {
        value: String,
        // not needed:
        // order: u8,
    }
    #[derive(Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    struct NotesInfoNote {
        note_id: NoteId,
        model_name: String,
        tags: Vec<String>,
        fields: HashMap<String, Field>, // not needed:
                                        // profile: String,
                                        // mod: u64,
                                        // cards: Vec<u64>,
    }
    #[derive(Serialize, Debug)]
    struct Query {
        query: String,
    }
    impl Request for Query {
        type Output = Vec<NotesInfoNote>;
        fn action_type() -> ActionType {
            ActionType::NotesInfo
        }
    }

    let request = Query {
        query: format!("\"deck:{}\"", &*DECK),
    };
    let result = request.request().await?;

    let notes = result
        .into_iter()
        .filter(|note| note.model_name == "Cloze")
        .map(|note| {
            (
                UpdateNote {
                    id: note.note_id,
                    fields: note.fields.into_iter().map(|(k, v)| (k, v.value)).collect(),
                    tags: note.tags,
                },
                false,
            )
        })
        .collect();
    *NOTES.lock()? = notes;

    Ok(())
}

#[derive(Error, Debug)]
pub enum UnseenNotesError {
    #[error("Failed to delete note: {0}")]
    Request(#[from] RequestError),
    #[error("Reading from stdin failed: {0}")]
    Stdin(#[from] std::io::Error),
    #[error("Failed to lock NOTES: {0}")]
    Lock(#[from] LockNotesError),
}
#[expect(clippy::await_holding_lock)] // fine, because it's the last thing we do
pub async fn handle_unseen_notes() -> Result<(), UnseenNotesError> {
    #[derive(Serialize, Debug)]
    #[serde(rename_all = "camelCase")]
    struct DeleteNotes {
        notes: Vec<NoteId>,
    }
    impl Request for DeleteNotes {
        fn action_type() -> ActionType {
            ActionType::DeleteNotes
        }
    }

    let mut buf = String::new();
    for (note, seen) in NOTES.lock()?.iter() {
        if !seen {
            println!(
                "Note present in Anki but not seen during run. Delete from Anki? (y/n)\n{note:?}"
            );
            loop {
                buf.clear();
                stdin().read_line(&mut buf)?;
                match buf.trim() {
                    "Y" | "y" | "Yes" | "yes" => {
                        let request = DeleteNotes {
                            notes: vec![note.id],
                        };
                        match request.request().await {
                            // return null, null on success
                            Err(RequestError::ErrorNorResult) => {}
                            Err(other) => Err(other)?,
                            Ok(_) => {}
                        }
                        break;
                    }
                    "N" | "n" | "No" | "no" => {
                        break;
                    }
                    other => println!("unknown option '{other}"),
                }
            }
        }
    }
    Ok(())
}

pub async fn add_cloze_note(text: String, tags: Vec<String>) -> Result<NoteId, RequestError> {
    #[derive(Serialize, Debug)]
    #[serde(rename_all = "camelCase")]
    enum DuplicateScope {
        Deck,
    }
    #[derive(Serialize, Debug)]
    #[serde(rename_all = "camelCase")]
    struct Options {
        allow_duplicate: bool,
        duplicate_scope: DuplicateScope,
    }
    #[derive(Serialize, Debug)]
    #[serde(rename_all = "camelCase")]
    struct AddNote {
        deck_name: String,
        model_name: String,
        fields: HashMap<String, String>,
        options: Options,
        tags: Vec<String>,
    }
    impl Request for AddNote {
        type Output = NoteId;
        fn action_type() -> ActionType {
            ActionType::AddNote
        }
    }

    ensure_deck_exists().await?;

    let add_note = AddNote {
        deck_name: DECK.clone(),
        model_name: "Cloze".to_string(),
        fields: HashMap::from([
            ("Text".to_string(), text.clone()),
            ("Back Extra".to_string(), String::new()),
        ]),
        options: Options {
            allow_duplicate: false,
            duplicate_scope: DuplicateScope::Deck,
        },
        tags: tags.clone(),
    };
    let request = Note { note: add_note };

    request.request().await
}

pub async fn update_cloze_note(
    text: String,
    id: NoteId,
    tags: Vec<String>,
) -> Result<(), RequestError> {
    let update_note = UpdateNote {
        fields: HashMap::from([
            ("Text".to_string(), text),
            ("Back Extra".to_string(), String::new()),
        ]),
        id,
        tags,
    };
    let request = Note { note: update_note };

    match request.request().await {
        // return null, null on success
        Err(RequestError::ErrorNorResult) => Ok(()),
        other => other,
    }
}

/// Ensures that the deck `DECK` exists
async fn ensure_deck_exists() -> Result<(), RequestError> {
    #[derive(Serialize, Debug)]
    #[serde(rename_all = "camelCase")]
    struct CreateDeck {
        deck: String,
    }
    impl Request for CreateDeck {
        type Output = u64;
        fn action_type() -> ActionType {
            ActionType::CreateDeck
        }
    }

    let request = CreateDeck { deck: DECK.clone() };
    request.request().await.map(|_: u64| {})
}
