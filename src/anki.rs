use log::{debug, warn};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::HashMap, fmt::Debug, io::stdin, sync::Mutex, time::Duration};
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

trait Request: Debug + Serialize {
    type Output: DeserializeOwned + Debug = ();
    fn action_type() -> ActionType;
    async fn request(&self) -> Result<Self::Output, String> {
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
                .expect("request should be cloneable")
                .send()
                .await
            {
                Ok(response) => break response,
                Err(e) if i < MAX_BACKOFF => {
                    warn!("AnkiConnect request failed (attempt {i}): {e}. Retrying in {timeout:?}");
                    sleep(timeout).await;
                }
                Err(e) => panic!("AnkiConnect request failed: {e}"),
            }
            i += 1;
        };

        let response = if response.status().is_success() {
            let response: Response<Self::Output> = response.json().await.unwrap();
            match (response.result, response.error) {
                (Some(result), None) => Ok(result),
                (None, Some(error)) => Err(error),
                (Some(_), Some(_)) => Err("Both error and result".to_string()),
                (None, None) => Err("Neither error nor result".to_string()),
            }
        } else {
            Err(format!("Error: Status: {}", response.status()))
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

pub async fn initialize_notes() {
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
    let result = request.request().await.expect("Request should'nt fail");

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
    *NOTES.lock().unwrap() = notes;
}

#[expect(clippy::await_holding_lock)] // fine, because it's the last thing we do
pub async fn handle_unseen_notes() {
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
    for (note, seen) in NOTES.lock().unwrap().iter() {
        if !seen {
            println!(
                "Note present in Anki but not seen during run. Delete from Anki? (Y/n)\n{note:?}"
            );
            stdin()
                .read_line(&mut buf)
                .expect("Reading from stdin shouldn't fail");
            let response = buf.trim();
            if response == "Y" || response == "y" {
                let request = DeleteNotes {
                    notes: vec![note.id],
                };
                match request.request().await {
                    // return null, null on success
                    Err(string) if &string == "Neither error nor result" => {}
                    Err(other) => panic!("{other:?}"),
                    _ => {}
                }
            }
            buf.clear();
        }
    }
}

pub async fn add_cloze_note(text: String, tags: Vec<String>) -> Result<NoteId, String> {
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

pub async fn update_cloze_note(text: String, id: NoteId, tags: Vec<String>) -> Result<(), String> {
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
        Err(string) if &string == "Neither error nor result" => Ok(()),
        other => other,
    }
}

/// Ensures that the deck `DECK` exists
async fn ensure_deck_exists() -> Result<(), String> {
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
