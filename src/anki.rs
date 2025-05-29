use log::debug;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::HashMap, fmt::Debug};

// Handles interaction with AnkiConnect.
// Could maybe use a bit more type-safety, stuff like action <-> params,
// and model <-> fields could be linked, but we dont really need it here and
// it would complicate the serialization

const DECK: &str = "Obsidian";

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Request<P: Serialize + Debug> {
    action: Action,
    version: u8,
    params: P,
}
impl<P: Serialize + Debug> Request<P> {
    async fn request<R: DeserializeOwned + Debug>(
        &self,
        client: &reqwest::Client,
    ) -> Result<R, String> {
        let response = client
            .post("http://localhost:8765")
            .json(&self)
            .send()
            .await
            .expect("AnkiConnect should be reachable");

        let response = if response.status().is_success() {
            let response: Response<R> = response.json().await.unwrap();
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
enum Action {
    AddNote,
    CreateDeck,
    UpdateNoteFields,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Note<T> {
    note: T,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CreateDeck {
    deck: String,
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

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct UpdateNode {
    id: NoteId,
    fields: HashMap<String, String>,
    tags: Vec<String>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Options {
    allow_duplicate: bool,
    duplicate_scope: DuplicateScope,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
enum DuplicateScope {
    Deck,
}

#[derive(Deserialize, Debug)]
struct Response<T> {
    result: Option<T>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(transparent)]
/// Contains a Unix Timestamp (so 13 decimal digits for the years 2001-2286)
pub struct NoteId(pub u64);

pub async fn add_cloze_note(
    text: String,
    tags: Vec<String>,
    client: &reqwest::Client,
) -> Result<NoteId, String> {
    ensure_deck_exists(client).await?;

    let note = AddNote {
        deck_name: DECK.to_string(),
        model_name: "Cloze".to_string(),
        fields: HashMap::from([
            ("Text".to_string(), text),
            ("Back Extra".to_string(), String::new()),
        ]),
        options: Options {
            allow_duplicate: false,
            duplicate_scope: DuplicateScope::Deck,
        },
        tags,
    };
    let request = Request {
        action: Action::AddNote,
        version: 6,
        params: Note { note },
    };

    request.request(client).await
}

pub async fn update_cloze_note(
    text: String,
    id: NoteId,
    tags: Vec<String>,
    client: &reqwest::Client,
) -> Result<(), String> {
    let note = UpdateNode {
        fields: HashMap::from([
            ("Text".to_string(), text),
            ("Back Extra".to_string(), String::new()),
        ]),
        id,
        tags,
    };
    let request = Request {
        action: Action::UpdateNoteFields,
        version: 6,
        params: Note { note },
    };

    match request.request(client).await {
        // return null, null on success
        Err(string) if &string == "Neither error nor result" => Ok(()),
        other => other,
    }
}

/// Ensures that the deck `DECK` exists
async fn ensure_deck_exists(client: &reqwest::Client) -> Result<(), String> {
    let request = Request {
        // create deck won't overwrite
        action: Action::CreateDeck,
        version: 6,
        params: CreateDeck {
            deck: DECK.to_string(),
        },
    };
    request.request(client).await.map(|_: u64| {})
}
