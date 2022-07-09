use anyhow::Context;
use curl::easy::Easy;
use rsa::pkcs1::DecodeRsaPrivateKey;
use sha2::Digest;
use std::time::SystemTime;

pub(crate) struct Github {
    key: rsa::RsaPrivateKey,
    id: u32,
    client: Easy,
}

pub(crate) struct RepositoryClient<'a> {
    github: &'a mut Github,
    repo: String,
    token: String,
}

impl Github {
    pub(crate) fn new(key: &str, id: u32) -> Github {
        Github {
            key: rsa::RsaPrivateKey::from_pkcs1_pem(key).unwrap(),
            id,
            client: Easy::new(),
        }
    }

    fn jwt(&self) -> String {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = serde_json::json! {{
            "iat": now - 10,
            "exp": now + 60,
            "iss": self.id,
        }};
        let header = r#"{"alg":"RS256","typ":"JWT"}"#;
        let payload = serde_json::to_string(&payload).unwrap();

        let encoding = base64::URL_SAFE_NO_PAD;
        let signature = self
            .key
            .sign(
                rsa::padding::PaddingScheme::PKCS1v15Sign {
                    hash: Some(rsa::hash::Hash::SHA2_256),
                },
                &sha2::Sha256::new()
                    .chain_update(format!(
                        "{}.{}",
                        base64::encode_config(&header, encoding),
                        base64::encode_config(&payload, encoding),
                    ))
                    .finalize(),
            )
            .unwrap();
        format!(
            "{}.{}.{}",
            base64::encode_config(&header, encoding),
            base64::encode_config(&payload, encoding),
            base64::encode_config(&signature, encoding),
        )
    }

    fn start_jwt_request(&mut self) -> anyhow::Result<()> {
        self.client.reset();
        self.client.useragent("rust-lang/promote-release").unwrap();
        let mut headers = curl::easy::List::new();
        headers.append(&format!("Authorization: Bearer {}", self.jwt()))?;
        self.client.http_headers(headers)?;
        Ok(())
    }

    pub(crate) fn token(&mut self, repository: &str) -> anyhow::Result<RepositoryClient<'_>> {
        self.start_jwt_request()?;
        self.client.get(true)?;
        self.client.url(&format!(
            "https://api.github.com/repos/{}/installation",
            repository
        ))?;
        #[derive(serde::Deserialize)]
        struct InstallationResponse {
            id: u32,
        }
        let installation_id = self
            .client
            .without_body()
            .send_with_response::<InstallationResponse>()?
            .id;

        self.start_jwt_request()?;
        self.client.post(true)?;
        self.client.url(&format!(
            "https://api.github.com/app/installations/{installation_id}/access_tokens"
        ))?;
        #[derive(serde::Deserialize)]
        struct TokenResponse {
            token: String,
        }
        let token = self
            .client
            .without_body()
            .send_with_response::<TokenResponse>()?
            .token;
        Ok(RepositoryClient {
            github: self,
            repo: repository.to_owned(),
            token,
        })
    }
}

impl RepositoryClient<'_> {
    fn start_new_request(&mut self) -> anyhow::Result<()> {
        self.github.client.reset();
        self.github.client.useragent("rust-lang/promote-release")?;
        let mut headers = curl::easy::List::new();
        headers.append(&format!("Authorization: token {}", self.token))?;
        self.github.client.http_headers(headers)?;
        Ok(())
    }

    pub(crate) fn tag(&mut self, tag: CreateTag<'_>) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct CreateTagInternal<'a> {
            tag: &'a str,
            message: &'a str,
            /// sha of the object being tagged
            object: &'a str,
            #[serde(rename = "type")]
            type_: &'a str,
            tagger: CreateTagTaggerInternal<'a>,
        }

        #[derive(serde::Serialize)]
        struct CreateTagTaggerInternal<'a> {
            name: &'a str,
            email: &'a str,
        }

        #[derive(serde::Serialize)]
        struct CreateRefInternal<'a> {
            #[serde(rename = "ref")]
            ref_: &'a str,
            sha: &'a str,
        }

        #[derive(serde::Deserialize)]
        struct CreatedTag {
            sha: String,
        }
        self.start_new_request()?;
        self.github.client.post(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repository}/git/tags",
            repository = self.repo,
        ))?;
        let created = self
            .github
            .client
            .with_body(CreateTagInternal {
                tag: tag.tag_name,
                message: tag.message,
                object: tag.commit,
                type_: "commit",
                tagger: CreateTagTaggerInternal {
                    name: tag.tagger_name,
                    email: tag.tagger_email,
                },
            })
            .send_with_response::<CreatedTag>()?;

        // This mostly exists to make sure the request is successful rather than
        // really checking the created ref (which we already know).
        #[derive(serde::Deserialize)]
        struct CreatedTagRef {
            #[serde(rename = "ref")]
            #[allow(unused)]
            ref_: String,
        }
        self.start_new_request()?;
        self.github.client.post(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repository}/git/refs",
            repository = self.repo,
        ))?;
        self.github
            .client
            .with_body(CreateRefInternal {
                ref_: &format!("refs/tags/{}", tag.tag_name),
                sha: &created.sha,
            })
            .send_with_response::<CreatedTagRef>()?;

        Ok(())
    }

    pub(crate) fn workflow_dispatch(&mut self, workflow: &str, branch: &str) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Request<'a> {
            #[serde(rename = "ref")]
            ref_: &'a str,
        }
        self.start_new_request()?;
        self.github.client.post(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repository}/actions/workflows/{workflow}/dispatches",
            repository = self.repo,
        ))?;

        self.github
            .client
            .with_body(Request { ref_: branch })
            .send()?;

        Ok(())
    }
}

#[derive(Copy, Clone)]
pub(crate) struct CreateTag<'a> {
    pub(crate) commit: &'a str,
    pub(crate) tag_name: &'a str,
    pub(crate) message: &'a str,
    pub(crate) tagger_name: &'a str,
    pub(crate) tagger_email: &'a str,
}

trait BodyExt {
    fn with_body<S>(&mut self, body: S) -> Request<'_, S>;
    fn without_body(&mut self) -> Request<'_, ()>;
}

impl BodyExt for Easy {
    fn with_body<S>(&mut self, body: S) -> Request<'_, S> {
        Request {
            body: Some(body),
            client: self,
        }
    }
    fn without_body(&mut self) -> Request<'_, ()> {
        Request {
            body: None,
            client: self,
        }
    }
}

struct Request<'a, S> {
    body: Option<S>,
    client: &'a mut Easy,
}

impl<S: serde::Serialize> Request<'_, S> {
    fn send_with_response<T: serde::de::DeserializeOwned>(self) -> anyhow::Result<T> {
        use std::io::Read;
        let mut response = Vec::new();
        let body = self.body.map(|body| serde_json::to_vec(&body).unwrap());
        {
            let mut transfer = self.client.transfer();
            // The unwrap in the read_function is basically guaranteed to not
            // happen: reading into a slice can't fail. We can't use `?` since the
            // return type inside transfer isn't compatible with io::Error.
            if let Some(mut body) = body.as_deref() {
                transfer.read_function(move |dest| Ok(body.read(dest).unwrap()))?;
            }
            transfer.write_function(|new_data| {
                response.extend_from_slice(new_data);
                Ok(new_data.len())
            })?;
            transfer.perform()?;
        }
        serde_json::from_slice(&response)
            .with_context(|| format!("{}", String::from_utf8_lossy(&response)))
    }

    fn send(self) -> anyhow::Result<()> {
        use std::io::Read;
        let body = self.body.map(|body| serde_json::to_vec(&body).unwrap());
        {
            let mut transfer = self.client.transfer();
            // The unwrap in the read_function is basically guaranteed to not
            // happen: reading into a slice can't fail. We can't use `?` since the
            // return type inside transfer isn't compatible with io::Error.
            if let Some(mut body) = body.as_deref() {
                transfer.read_function(move |dest| Ok(body.read(dest).unwrap()))?;
            }
            transfer.perform()?;
        }

        Ok(())
    }
}
