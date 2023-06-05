use anyhow::Error;
use curl::easy::Easy;

pub struct Fastly {
    api_token: String,
    domain: String,
    client: Easy,
}

impl Fastly {
    pub fn new(api_token: String, domain: String) -> Self {
        Self {
            api_token,
            domain,
            client: Easy::new(),
        }
    }

    pub fn purge(&mut self, path: &str) -> Result<(), Error> {
        self.start_new_request()?;

        self.client.post(true)?;
        self.client.url(&format!(
            "https://api.fastly.com/purge/{}/{}",
            self.domain, path
        ))?;

        self.client.perform().map_err(|error| error.into())
    }

    fn start_new_request(&mut self) -> anyhow::Result<()> {
        self.client.reset();
        self.client.useragent("rust-lang/promote-release")?;
        let mut headers = curl::easy::List::new();
        headers.append(&format!("Fastly-Key: {}", self.api_token))?;
        headers.append("Content-Type: application/json")?;
        self.client.http_headers(headers)?;
        Ok(())
    }
}
