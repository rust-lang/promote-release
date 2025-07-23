use anyhow::Error;
use curl::easy::Easy;

pub struct Fastly {
    api_token: String,
    service_id: String,
    client: Easy,
}

impl Fastly {
    pub fn new(api_token: String, service_id: String) -> Self {
        Self {
            api_token,
            service_id,
            client: Easy::new(),
        }
    }

    pub fn purge(&mut self, path: &str) -> Result<(), Error> {
        let surrogate_key = path_to_surrogate_key(path);
        let url = format!(
            "https://api.fastly.com/service/{}/purge/{}",
            self.service_id, surrogate_key
        );

        self.start_new_request()?;

        self.client.post(true)?;
        self.client.url(&url)?;

        println!("invalidating Fastly cache with POST '{url}'");

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

fn path_to_surrogate_key(path: &str) -> String {
    path.chars().filter(|c| c.is_alphanumeric()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_to_surrogate_key_dist() {
        let path = "/dist/*";

        let surrogate_key = path_to_surrogate_key(path);

        assert_eq!("dist", surrogate_key);
    }
}
