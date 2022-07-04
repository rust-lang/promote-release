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
        let installation_id = send_request::<InstallationResponse>(&mut self.client)?.id;

        self.start_jwt_request()?;
        self.client.post(true)?;
        self.client.url(&format!(
            "https://api.github.com/app/installations/{installation_id}/access_tokens"
        ))?;
        #[derive(serde::Deserialize)]
        struct TokenResponse {
            token: String,
        }
        let token = send_request::<TokenResponse>(&mut self.client)?.token;
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
        let mut headers = curl::easy::List::new();
        headers.append(&format!("Authorization: token {}", self.token))?;
        self.github.client.http_headers(headers)?;
        Ok(())
    }

    pub(crate) fn tag(&mut self, tag: CreateTag<'_>) -> anyhow::Result<()> {
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
        let created = send_request_body::<CreatedTag, _>(
            &mut self.github.client,
            CreateTagInternal {
                tag: tag.tag_name,
                message: tag.message,
                object: tag.commit,
                type_: "commit",
                tagger: CreateTagTaggerInternal {
                    name: tag.tagger_name,
                    email: tag.tagger_email,
                },
            },
        )?;

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
        send_request_body::<CreatedTagRef, _>(
            &mut self.github.client,
            CreateRefInternal {
                ref_: &format!("refs/tags/{}", tag.tag_name),
                sha: &created.sha,
            },
        )?;

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

fn send_request_body<T: serde::de::DeserializeOwned, S: serde::Serialize>(
    client: &mut Easy,
    body: S,
) -> anyhow::Result<T> {
    use std::io::Read;
    client.useragent("rust-lang/promote-release").unwrap();
    let mut response = Vec::new();
    let body = serde_json::to_vec(&body).unwrap();
    {
        let mut transfer = client.transfer();
        let mut body = &body[..];
        // The unwrap in the read_function is basically guaranteed to not
        // happen: reading into a slice can't fail. We can't use `?` since the
        // return type inside transfer isn't compatible with io::Error.
        transfer.read_function(move |dest| Ok(body.read(dest).unwrap()))?;
        transfer.write_function(|new_data| {
            response.extend_from_slice(new_data);
            Ok(new_data.len())
        })?;
        transfer.perform()?;
    }
    serde_json::from_slice(&response)
        .with_context(|| format!("{}", String::from_utf8_lossy(&response)))
}

fn send_request<T: serde::de::DeserializeOwned>(client: &mut Easy) -> anyhow::Result<T> {
    client.useragent("rust-lang/promote-release").unwrap();
    let mut response = Vec::new();
    {
        let mut transfer = client.transfer();
        transfer.write_function(|new_data| {
            response.extend_from_slice(new_data);
            Ok(new_data.len())
        })?;
        transfer.perform()?;
    }
    serde_json::from_slice(&response)
        .with_context(|| format!("{}", String::from_utf8_lossy(&response)))
}
