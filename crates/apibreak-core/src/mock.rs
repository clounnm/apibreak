//! Recorded responses, stored as editable JSON files.

use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::body;
use crate::error::{Error, Result};
use crate::scenario::Endpoint;
use crate::store::{Exchange, Headers, Outcome};

const MOCKS_DIR: &str = "mocks";

/// Headers that describe the recorded transfer rather than the content.
const SKIPPED_HEADERS: [&str; 5] = [
    "content-encoding",
    "content-length",
    "transfer-encoding",
    "date",
    "vary",
];

/// A saved response. JSON bodies are kept as JSON so they are easy to edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mock {
    pub status: u16,
    #[serde(default)]
    pub headers: Headers,
    #[serde(default)]
    pub body: Value,
}

impl Mock {
    /// Builds a mock from a finished exchange. Refuses anything that would
    /// not reproduce the response faithfully.
    pub fn from_exchange(exchange: &Exchange) -> Result<Self> {
        let refuse = |reason: &str| Error::CannotRecord(reason.to_owned());
        if exchange.scenario.is_some() {
            return Err(refuse(
                "this response came from a scenario, not the backend",
            ));
        }
        let Outcome::Response(response) = &exchange.outcome else {
            return Err(refuse("the request has no response"));
        };
        let decoded = body::decode(&response.headers, &response.body);
        if !decoded.complete {
            return Err(refuse("the body is too large to record in full"));
        }
        Ok(Self {
            status: response.status,
            headers: response
                .headers
                .iter()
                .filter(|(name, _)| {
                    let name = name.to_ascii_lowercase();
                    !SKIPPED_HEADERS.contains(&name.as_str())
                        && !name.starts_with("access-control-")
                })
                .cloned()
                .collect(),
            body: body_value(decoded.bytes)
                .ok_or_else(|| refuse("binary bodies are not supported"))?,
        })
    }

    pub fn parse(file: &str, bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|source| Error::InvalidMock {
            file: file.to_owned(),
            source,
        })
    }

    /// The bytes to send: objects and arrays as JSON, strings as they are.
    pub fn body_bytes(&self) -> Bytes {
        match &self.body {
            Value::Null => Bytes::new(),
            Value::String(text) => Bytes::from(text.clone()),
            other => Bytes::from(other.to_string()),
        }
    }
}

/// `None` for binary data. Only containers count as JSON: the body `"a"`
/// must come back with its quotes, which a JSON string would lose.
fn body_value(bytes: Vec<u8>) -> Option<Value> {
    if let Ok(value @ (Value::Object(_) | Value::Array(_))) = serde_json::from_slice(&bytes) {
        return Some(value);
    }
    if bytes.is_empty() {
        return Some(Value::Null);
    }
    String::from_utf8(bytes).ok().map(Value::String)
}

/// Finds mock files under the configuration directory.
#[derive(Debug, Clone)]
pub struct MockStore {
    root: PathBuf,
}

impl MockStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The path of `endpoint`'s mock, relative to the configuration directory.
    pub fn file_for(endpoint: &Endpoint) -> String {
        let slug: Vec<&str> = endpoint
            .path
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|part| !part.is_empty())
            .collect();
        let slug = if slug.is_empty() {
            "root".to_owned()
        } else {
            slug.join("_")
        };
        format!("{MOCKS_DIR}/{}_{slug}.json", endpoint.method)
    }

    /// Resolves a relative mock path. Scenario files get committed and
    /// shared, so a path must not reach outside the configuration directory.
    pub fn resolve(&self, file: &str) -> Result<PathBuf> {
        let inside = Path::new(file)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
        if !inside {
            return Err(Error::InvalidScenario(format!(
                "mock path `{file}` must stay inside the config directory"
            )));
        }
        Ok(self.root.join(file))
    }

    pub fn exists(&self, file: &str) -> bool {
        self.resolve(file).is_ok_and(|path| path.is_file())
    }

    /// Writes `mock` for `endpoint` and returns its relative path.
    pub fn save(&self, endpoint: &Endpoint, mock: &Mock) -> Result<String> {
        let file = Self::file_for(endpoint);
        let path = self.resolve(&file)?;
        let directory = self.root.join(MOCKS_DIR);
        std::fs::create_dir_all(&directory).map_err(|source| Error::Io {
            action: "creating",
            path: directory,
            source,
        })?;
        let mut json = serde_json::to_vec_pretty(mock).map_err(|source| Error::InvalidMock {
            file: file.clone(),
            source,
        })?;
        json.push(b'\n');
        std::fs::write(&path, json).map_err(|source| Error::Io {
            action: "writing",
            path,
            source,
        })?;
        Ok(file)
    }

    /// Reads a mock, picking up any edit made since the last request.
    pub async fn load(&self, file: &str) -> Result<Mock> {
        let path = self.resolve(file)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => Mock::parse(file, &bytes),
            Err(source) => Err(Error::Io {
                action: "reading",
                path,
                source,
            }),
        }
    }

    /// Checks a mock file on disk, as after hand edits.
    pub fn validate(&self, file: &str) -> Result<()> {
        let path = self.resolve(file)?;
        let bytes = std::fs::read(&path).map_err(|source| Error::Io {
            action: "reading",
            path,
            source,
        })?;
        Mock::parse(file, &bytes).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use chrono::Local;
    use flate2::{write::GzEncoder, Compression};

    use super::*;
    use crate::store::{Captured, ResponseRecord};

    fn exchange(headers: Headers, body: Vec<u8>) -> Exchange {
        Exchange {
            id: 1,
            started_at: Local::now(),
            method: "GET".into(),
            path: "/a".into(),
            uri: "/a".into(),
            request_headers: Vec::new(),
            request_body: Captured::default(),
            outcome: Outcome::Response(ResponseRecord {
                status: 200,
                headers,
                body: Captured::new(Bytes::from(body)),
            }),
            duration: None,
            scenario: None,
        }
    }

    fn endpoint(text: &str) -> Endpoint {
        text.parse().expect("endpoint")
    }

    #[test]
    fn names_files_after_method_and_path() {
        for (text, file) in [
            ("GET /api/users", "mocks/GET_api_users.json"),
            ("GET /api/users/:id", "mocks/GET_api_users_id.json"),
            ("POST /", "mocks/POST_root.json"),
            ("GET /../../etc", "mocks/GET_etc.json"),
        ] {
            assert_eq!(MockStore::file_for(&endpoint(text)), file);
        }
    }

    #[test]
    fn rejects_paths_outside_the_config_dir() {
        let store = MockStore::new("/config");
        for file in ["../secret", "/etc/passwd", "mocks/../../x"] {
            assert!(store.resolve(file).is_err(), "{file}");
        }
        assert_eq!(
            store.resolve("mocks/a.json").expect("inside"),
            Path::new("/config/mocks/a.json")
        );
    }

    #[test]
    fn records_gzip_json_decoded_without_transfer_headers() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(br#"{"b":1,"a":2}"#).expect("write");
        let headers = vec![
            ("content-encoding".to_owned(), "gzip".to_owned()),
            ("content-length".to_owned(), "9".to_owned()),
            ("set-cookie".to_owned(), "a=1".to_owned()),
            ("access-control-allow-origin".to_owned(), "x".to_owned()),
        ];

        let mock =
            Mock::from_exchange(&exchange(headers, encoder.finish().expect("gzip"))).expect("mock");

        assert_eq!(mock.headers, [("set-cookie".to_owned(), "a=1".to_owned())]);
        assert_eq!(&mock.body_bytes()[..], br#"{"b":1,"a":2}"#);
    }

    #[test]
    fn plain_text_round_trips_exactly() {
        for text in ["hello", "\"quoted\"", "42", ""] {
            let mock =
                Mock::from_exchange(&exchange(Vec::new(), text.as_bytes().to_vec())).expect("mock");
            assert_eq!(&mock.body_bytes()[..], text.as_bytes());
        }
    }

    #[test]
    fn refuses_what_it_cannot_reproduce() {
        let mut scenario = exchange(Vec::new(), b"x".to_vec());
        scenario.scenario = Some("500".into());
        assert!(matches!(
            Mock::from_exchange(&scenario),
            Err(Error::CannotRecord(_))
        ));

        let mut failed = exchange(Vec::new(), Vec::new());
        failed.outcome = Outcome::Failed("x".into());
        assert!(Mock::from_exchange(&failed).is_err());

        let binary = exchange(Vec::new(), vec![0xff, 0xfe]);
        assert!(Mock::from_exchange(&binary).is_err());

        let long = exchange(Vec::new(), vec![b'a'; crate::store::CAPTURE_LIMIT + 1]);
        assert!(Mock::from_exchange(&long).is_err());
    }

    #[tokio::test]
    async fn saves_and_loads_from_disk() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = MockStore::new(directory.path());
        let mock = Mock::from_exchange(&exchange(Vec::new(), b"[1]".to_vec())).expect("mock");

        let file = store.save(&endpoint("GET /a"), &mock).expect("save");

        assert!(store.exists(&file));
        assert_eq!(store.load(&file).await.expect("load"), mock);
    }

    #[test]
    fn validate_reports_broken_edits() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = MockStore::new(directory.path());
        std::fs::write(directory.path().join("bad.json"), "{").expect("write");
        assert!(matches!(
            store.validate("bad.json"),
            Err(Error::InvalidMock { .. })
        ));
    }
}
