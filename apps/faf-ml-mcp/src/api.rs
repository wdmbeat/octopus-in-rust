//! Thin REST client for the faf-ml-server API.

use anyhow::{anyhow, Context};
use serde::{de::DeserializeOwned, Serialize};

/// Base URL of the faf-ml-server API (env `FAF_ML_API`, default
/// `http://localhost:3100`).
#[derive(Clone)]
pub struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    pub fn from_env() -> Self {
        let base =
            std::env::var("FAF_ML_API").unwrap_or_else(|_| "http://localhost:3100".to_string());
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Turn a non-success response into an error carrying status + body.
    async fn check(resp: reqwest::Response) -> anyhow::Result<reqwest::Response> {
        if resp.status().is_success() {
            Ok(resp)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(anyhow!("HTTP {status}: {body}"))
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let resp = Self::check(self.http.get(self.url(path)).send().await?).await?;
        resp.json::<T>().await.context("decoding response")
    }

    pub async fn post_json<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
    ) -> anyhow::Result<Resp> {
        let resp = Self::check(self.http.post(self.url(path)).json(body).send().await?).await?;
        resp.json::<Resp>().await.context("decoding response")
    }

    pub async fn patch_json<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
    ) -> anyhow::Result<Resp> {
        let resp = Self::check(self.http.patch(self.url(path)).json(body).send().await?).await?;
        resp.json::<Resp>().await.context("decoding response")
    }

    /// DELETE, returning the response body text (often a summary message).
    pub async fn delete(&self, path: &str) -> anyhow::Result<String> {
        let resp = Self::check(self.http.delete(self.url(path)).send().await?).await?;
        Ok(resp.text().await.unwrap_or_default())
    }

    /// Multipart PNG upload (`POST /api/screenshots`).
    pub async fn upload<T: DeserializeOwned>(&self, paths: &[String]) -> anyhow::Result<T> {
        let mut form = reqwest::multipart::Form::new();
        for path in paths {
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("reading {path}"))?;
            let filename = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("upload.png")
                .to_string();
            form = form.part(
                "files",
                reqwest::multipart::Part::bytes(bytes).file_name(filename),
            );
        }
        let resp = Self::check(
            self.http
                .post(self.url("/api/screenshots"))
                .multipart(form)
                .send()
                .await?,
        )
        .await?;
        resp.json::<T>().await.context("decoding response")
    }
}
