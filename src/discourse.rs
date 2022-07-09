use crate::curl_helper::BodyExt;
use curl::easy::Easy;

pub struct Discourse {
    root: String,
    api_key: String,
    api_username: String,
    client: Easy,
}

impl Discourse {
    pub fn new(root: String, api_username: String, api_key: String) -> Discourse {
        Discourse {
            root,
            api_key,
            api_username,
            client: Easy::new(),
        }
    }

    fn start_new_request(&mut self) -> anyhow::Result<()> {
        self.client.reset();
        self.client.useragent("rust-lang/promote-release")?;
        let mut headers = curl::easy::List::new();
        headers.append(&format!("Api-Key: {}", self.api_key))?;
        headers.append(&format!("Api-Username: {}", self.api_username))?;
        headers.append("Content-Type: application/json")?;
        self.client.http_headers(headers)?;
        Ok(())
    }

    /// Returns a URL to the topic
    pub fn create_topic(
        &mut self,
        category: u32,
        title: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        #[derive(serde::Serialize)]
        struct Request<'a> {
            title: &'a str,
            #[serde(rename = "raw")]
            body: &'a str,
            category: u32,
            archetype: &'a str,
        }
        #[derive(serde::Deserialize)]
        struct Response {
            topic_id: u32,
            topic_slug: String,
        }
        self.start_new_request()?;
        self.client.post(true)?;
        self.client.url(&format!("{}/posts.json", self.root))?;
        let resp = self
            .client
            .with_body(Request {
                title,
                body,
                category,
                archetype: "regular",
            })
            .send_with_response::<Response>()?;
        Ok(format!(
            "{}/t/{}/{}",
            self.root, resp.topic_slug, resp.topic_id
        ))
    }
}
