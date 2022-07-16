use crate::curl_helper::BodyExt;
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

        self.create_ref(&format!("refs/tags/{}", tag.tag_name), &created.sha)?;

        Ok(())
    }

    /// Returns the SHA of the tip of this ref, if it exists.
    pub(crate) fn get_ref(&mut self, name: &str) -> anyhow::Result<String> {
        // This mostly exists to make sure the request is successful rather than
        // really checking the created ref (which we already know).
        #[derive(serde::Deserialize)]
        struct Reference {
            object: Object,
        }
        #[derive(serde::Deserialize)]
        struct Object {
            sha: String,
        }

        self.start_new_request()?;
        self.github.client.get(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repository}/git/ref/{name}",
            repository = self.repo,
        ))?;
        Ok(self
            .github
            .client
            .without_body()
            .send_with_response::<Reference>()?
            .object
            .sha)
    }

    pub(crate) fn create_ref(&mut self, name: &str, sha: &str) -> anyhow::Result<()> {
        // This mostly exists to make sure the request is successful rather than
        // really checking the created ref (which we already know).
        #[derive(serde::Deserialize)]
        struct CreatedTagRef {
            #[serde(rename = "ref")]
            #[allow(unused)]
            ref_: String,
        }
        #[derive(serde::Serialize)]
        struct CreateRefInternal<'a> {
            #[serde(rename = "ref")]
            name: &'a str,
            sha: &'a str,
        }

        self.start_new_request()?;
        self.github.client.post(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repository}/git/refs",
            repository = self.repo,
        ))?;
        self.github
            .client
            .with_body(CreateRefInternal { name, sha })
            .send_with_response::<CreatedTagRef>()?;

        Ok(())
    }

    pub(crate) fn update_ref(&mut self, name: &str, sha: &str, force: bool) -> anyhow::Result<()> {
        // This mostly exists to make sure the request is successful rather than
        // really checking the created ref (which we already know).
        #[derive(serde::Deserialize)]
        struct CreatedRef {
            #[serde(rename = "ref")]
            #[allow(unused)]
            ref_: String,
        }
        #[derive(serde::Serialize)]
        struct UpdateRefInternal<'a> {
            sha: &'a str,
            force: bool,
        }

        self.start_new_request()?;
        // We want curl to read the request body, so configure POST.
        self.github.client.post(true)?;
        // However, the actual request should be a PATCH request.
        self.github.client.custom_request("PATCH")?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repository}/git/refs/{name}",
            repository = self.repo,
        ))?;
        self.github
            .client
            .with_body(UpdateRefInternal { sha, force })
            .send_with_response::<CreatedRef>()?;

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

    /// Note that this API *will* fail if the file already exists in this
    /// branch; we don't update existing files.
    pub(crate) fn create_file(
        &mut self,
        branch: &str,
        path: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Request<'a> {
            message: &'a str,
            content: &'a str,
            branch: &'a str,
        }
        self.start_new_request()?;
        self.github.client.put(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repository}/contents/{path}",
            repository = self.repo,
        ))?;
        self.github
            .client
            .with_body(Request {
                branch,
                message: "Creating file via promote-release automation",
                content: &base64::encode(&content),
            })
            .send()?;
        Ok(())
    }

    pub(crate) fn create_pr(
        &mut self,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Request<'a> {
            head: &'a str,
            base: &'a str,
            title: &'a str,
            body: &'a str,
        }
        self.start_new_request()?;
        self.github.client.post(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repository}/pulls",
            repository = self.repo,
        ))?;
        self.github
            .client
            .with_body(Request {
                base,
                head,
                title,
                body,
            })
            .send()?;
        Ok(())
    }

    /// Returns the last commit (SHA) on a repository's default branch which changed
    /// the passed path.
    pub(crate) fn last_commit_for_file(&mut self, path: &str) -> anyhow::Result<String> {
        #[derive(serde::Deserialize)]
        struct CommitData {
            sha: String,
        }
        self.start_new_request()?;
        self.github.client.get(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repo}/commits?path={path}",
            repo = self.repo
        ))?;
        let mut commits = self
            .github
            .client
            .without_body()
            .send_with_response::<Vec<CommitData>>()?;
        if commits.is_empty() {
            anyhow::bail!("No commits for path {:?}", path);
        }
        Ok(commits.remove(0).sha)
    }

    /// Returns the contents of the file
    pub(crate) fn read_file(&mut self, sha: Option<&str>, path: &str) -> anyhow::Result<GitFile> {
        self.start_new_request()?;
        self.github.client.get(true)?;
        self.github.client.url(&format!(
            "https://api.github.com/repos/{repo}/contents/{path}{maybe_ref}",
            repo = self.repo,
            maybe_ref = sha.map(|s| format!("?ref={}", s)).unwrap_or_default()
        ))?;
        self.github
            .client
            .without_body()
            .send_with_response::<GitFile>()
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum GitFile {
    File { encoding: String, content: String },
    Submodule { sha: String },
}

impl GitFile {
    pub(crate) fn submodule_sha(&self) -> &str {
        if let GitFile::Submodule { sha } = self {
            sha
        } else {
            panic!("{:?} not a submodule", self);
        }
    }

    pub(crate) fn content(&self) -> anyhow::Result<String> {
        if let GitFile::File { encoding, content } = self {
            assert_eq!(encoding, "base64");
            Ok(String::from_utf8(base64::decode(&content.trim())?)?)
        } else {
            panic!("content() on {:?}", self);
        }
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
