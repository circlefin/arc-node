use std::time::Duration;

use futures_util::future::join_all;
use url::Url;

pub async fn fetch_all_metrics(metrics_urls: &[(String, Url)]) -> Vec<(String, String)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    let futures = metrics_urls.iter().map(|(name, url)| {
        let client = client.clone();
        let name = name.clone();
        let url = url.clone();
        async move {
            let body = match client.get(url.as_str()).send().await {
                Ok(resp) => resp.text().await.unwrap_or_default(),
                Err(_) => String::new(),
            };
            (name, body)
        }
    });

    join_all(futures).await
}
