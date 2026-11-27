use std::time::{Duration, Instant, SystemTime};

use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CACHE_CONTROL, ETAG, IF_NONE_MATCH, RETRY_AFTER,
};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode, Url};
use serde::{de::DeserializeOwned, Serialize};

use crate::content::verify_download;
use crate::trace::{submit_traceparent, OtlpTrace, RequestTraceSnapshot, TraceBaseline};
use crate::{
    valid_id, ArtifactRef, AudioProviderResult, Catalog, ChatOptions, DiagnosticRequestPage, Error, ImageProviderResult,
    ListOptions, MeshProviderResult, Message, ModelCatalog, PolicySetting, Provider, ProviderPath,
    ProviderResult, Quote, QuoteInput, QuoteOptions, Receipt, RequestDiagnostic, RequestEvents, RequestFilters,
    RequestPage, RequestStatus, RequestTrace, RoutePreview, RoutePreviewOptions, SubmitOptions,
    UsageOptions, UsageReport, VerifiedArtifact, VideoProviderResult, WaitOptions,
};

#[derive(Clone)]
pub struct TokenCraftClient {
    origin: Url,
    bearer: Option<HeaderValue>,
    http: Client,
    app_ref: Option<String>,
    submit_traceparent: Option<HeaderValue>,
    timeout: Duration,
    submit_max_attempts: u8,
    download_max_attempts: u8,
}

impl TokenCraftClient {
    pub fn new(base_url: &str, api_key: Option<&str>) -> Result<Self, Error> {
        Self::with_timeout(base_url, api_key, Duration::from_secs(30))
    }

    pub fn with_timeout(
        base_url: &str,
        api_key: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        validate_duration(timeout)?;
        let invalid_url = || {
            Error::InvalidInput(
                "use HTTPS or loopback HTTP without URL credentials, query or fragment",
            )
        };
        let mut origin = Url::parse(base_url).map_err(|_| invalid_url())?;
        let loopback = matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(origin.scheme() == "https" || (origin.scheme() == "http" && loopback))
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(invalid_url());
        }
        if !origin.path().ends_with('/') {
            origin
                .path_segments_mut()
                .map_err(|_| invalid_url())?
                .push("");
        }

        let bearer = api_key
            .map(|value| {
                let invalid = || Error::InvalidInput("supply a valid TokenCraft API key");
                if value.is_empty()
                    || value.len() > 512
                    || !value.bytes().all(|byte| byte.is_ascii_graphic())
                {
                    return Err(invalid());
                }
                let mut header =
                    HeaderValue::from_str(&format!("Bearer {value}")).map_err(|_| invalid())?;
                header.set_sensitive(true);
                Ok(header)
            })
            .transpose()?;

        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(timeout);
        if loopback {
            builder = builder.no_proxy();
        }
        let http = builder
            .build()
            .map_err(|source| Error::from_http(source, None))?;
        Ok(Self {
            origin,
            bearer,
            http,
            app_ref: None,
            submit_traceparent: None,
            timeout,
            submit_max_attempts: 1,
            download_max_attempts: 1,
        })
    }

    pub async fn catalog(&self) -> Result<Catalog, Error> {
        self.send(self.request(Method::GET, "v1/catalog/offers", false)?, None)
            .await
    }

    /// Discover models in the key's project, or the public catalog without a key.
    pub async fn models(&self) -> Result<ModelCatalog, Error> {
        match self.bearer {
            Some(_) => self.models_scoped().await,
            None => self.models_public().await,
        }
    }

    pub async fn models_public(&self) -> Result<ModelCatalog, Error> {
        self.send(self.request(Method::GET, "v1/catalog/models", false)?, None).await
    }

    /// Requires a key that can submit project requests.
    pub async fn models_scoped(&self) -> Result<ModelCatalog, Error> {
        self.send(self.request(Method::GET, "v1/catalog/models/scoped", true)?, None).await
    }

    pub async fn quote(&self, input: &QuoteInput) -> Result<Quote, Error> {
        match self.bearer {
            Some(_) => self.quote_scoped(input).await,
            None => self.quote_public(input).await,
        }
    }

    /// Request an ordered provider preference. Pass an empty list to select by lowest estimate.
    pub async fn quote_with_preferred_providers(
        &self,
        input: &QuoteInput,
        providers: &[Provider],
    ) -> Result<Quote, Error> {
        self.quote_with_options(
            input,
            QuoteOptions {
                preferred_providers: Some(providers),
                ..QuoteOptions::default()
            },
        )
        .await
    }

    /// Quote with request-level routing requirements or project attribution.
    pub async fn quote_with_options(
        &self,
        input: &QuoteInput,
        options: QuoteOptions<'_>,
    ) -> Result<Quote, Error> {
        let scoped = self.bearer.is_some();
        if !scoped && (options.client_ref.is_some() || options.pinned_offer_id.is_some()) {
            return Err(Error::MissingKey);
        }
        let path = if scoped { "v1/quotes/scoped" } else { "v1/quotes" };
        let mut request = self.request(Method::POST, path, scoped)?;
        if let Some(offer) = options.pinned_offer_id {
            request = request.query(&[("pinned_offer_id", offer)]);
        }
        if let Some(client) = options.client_ref {
            request = request.query(&[("client_ref", client)]);
        }
        self.send(request.json(&QuoteBody { input, options }), None).await
    }

    pub async fn quote_public(&self, input: &QuoteInput) -> Result<Quote, Error> {
        let request = self.request(Method::POST, "v1/quotes", false)?.json(input);
        self.send(request, None).await
    }

    pub async fn quote_scoped(&self, input: &QuoteInput) -> Result<Quote, Error> {
        let request = self
            .request(Method::POST, "v1/quotes/scoped", true)?
            .json(input);
        self.send(request, None).await
    }

    /// Quote using the same client attribution that will be sent with the request.
    pub async fn quote_scoped_for_client(
        &self,
        input: &QuoteInput,
        client_ref: &str,
    ) -> Result<Quote, Error> {
        self.quote_with_options(
            input,
            QuoteOptions {
                client_ref: Some(client_ref),
                ..QuoteOptions::default()
            },
        )
        .await
    }

    /// Quote exactly one eligible gateway offer. Submitting the quote preserves the pin.
    pub async fn quote_offer(
        &self,
        input: &QuoteInput,
        offer_id: &str,
        client_ref: Option<&str>,
    ) -> Result<Quote, Error> {
        self.quote_with_options(
            input,
            QuoteOptions {
                client_ref,
                pinned_offer_id: Some(offer_id),
                ..QuoteOptions::default()
            },
        )
        .await
    }

    /// Set the application reference on submissions made by this client.
    pub fn with_app_ref(mut self, app_ref: impl Into<String>) -> Self {
        self.app_ref = Some(app_ref.into());
        self
    }

    /// Link native submissions to a caller span. Clone a shared client for each caller context.
    pub fn with_submit_traceparent(mut self, value: &str) -> Result<Self, Error> {
        self.submit_traceparent = Some(submit_traceparent(value)?);
        Ok(self)
    }

    /// Allow up to 8 attempts on native submissions and workflow invocations, including the first.
    ///
    /// Retries share the client's timeout and reuse the same body and idempotency key.
    /// They cover interrupted transfers and HTTP 429 or 503 with a valid Retry-After.
    pub fn with_submit_max_attempts(mut self, attempts: u8) -> Result<Self, Error> {
        if !(1..=8).contains(&attempts) {
            return Err(Error::InvalidInput("supply 1 to 8 submission attempts"));
        }
        self.submit_max_attempts = attempts;
        Ok(self)
    }

    /// Allow up to 8 attempts for artifact and workflow output downloads, including the first.
    ///
    /// Retries cover interrupted transfers and HTTP 429 or 503 with a valid Retry-After.
    /// All attempts and the final verified transfer share the client's timeout.
    /// The default is one attempt.
    pub fn with_download_max_attempts(mut self, attempts: u8) -> Result<Self, Error> {
        if !(1..=8).contains(&attempts) {
            return Err(Error::InvalidInput("supply 1 to 8 download attempts"));
        }
        self.download_max_attempts = attempts;
        Ok(self)
    }

    pub async fn submit(
        &self,
        quote: &Quote,
        options: SubmitOptions<'_>,
    ) -> Result<Receipt, Error> {
        self.submit_request(quote, options, MediaPrompts::default())
            .await
    }

    pub async fn submit_video(
        &self,
        quote: &Quote,
        video_prompt: &str,
        options: SubmitOptions<'_>,
    ) -> Result<Receipt, Error> {
        self.submit_request(
            quote,
            options,
            MediaPrompts {
                video_prompt: Some(video_prompt),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn submit_audio(
        &self,
        quote: &Quote,
        audio_prompt: &str,
        options: SubmitOptions<'_>,
    ) -> Result<Receipt, Error> {
        self.submit_request(
            quote,
            options,
            MediaPrompts {
                audio_prompt: Some(audio_prompt),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn submit_mesh(
        &self,
        quote: &Quote,
        mesh_prompt: &str,
        options: SubmitOptions<'_>,
    ) -> Result<Receipt, Error> {
        self.submit_request(
            quote,
            options,
            MediaPrompts {
                mesh_prompt: Some(mesh_prompt),
                ..Default::default()
            },
        )
        .await
    }

    async fn submit_request(
        &self,
        quote: &Quote,
        options: SubmitOptions<'_>,
        prompts: MediaPrompts<'_>,
    ) -> Result<Receipt, Error> {
        let key = options.idempotency_key;
        validate_idempotency_key(key)?;
        let input = &quote.input;
        let routing = quote.routing.as_ref();
        // Older saved Rust quotes only retained explicit precedence settings under routing.
        let quote_options = QuoteOptions {
            preferred_providers: input.preferred_providers.as_deref().or_else(|| {
                routing
                    .and_then(|policy| policy.provider_preference.as_ref())
                    .and_then(PolicySetting::request_value)
                    .map(Vec::as_slice)
            }),
            min_quality: input.min_quality.or_else(|| {
                routing
                    .and_then(|policy| policy.min_quality.as_ref())
                    .and_then(PolicySetting::request_value)
                    .copied()
            }),
            min_usage_accounts: input.min_usage_accounts.or_else(|| {
                routing
                    .and_then(|policy| policy.min_usage_accounts.as_ref())
                    .and_then(PolicySetting::request_value)
                    .copied()
            }),
            data_requirements: input.data_requirements.as_ref(),
            ..QuoteOptions::default()
        };
        let body = SubmitBody {
            quote_id: &quote.id,
            pinned_offer_id: quote.pinned_offer_id.as_deref(),
            input: QuoteBody {
                input: &input.base,
                options: quote_options,
            },
            client_ref: options.client_ref,
            app_ref: self.app_ref.as_deref(),
            delivery: options
                .webhook_subscription_id
                .map(|webhook_subscription_id| Delivery {
                    webhook_subscription_id,
                }),
            messages: options.messages,
            image_prompt: options.image_prompt,
            prompts,
            chat_options: options.chat_options,
        };
        let mut request = self
            .request(Method::POST, "v1/requests", true)?
            .header("Idempotency-Key", key)
            .json(&body);
        if let Some(traceparent) = &self.submit_traceparent {
            request = request.header("traceparent", traceparent.clone());
        }
        self.send_idempotent(request).await
    }

    pub(super) async fn send_idempotent<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, Error> {
        if self.submit_max_attempts == 1 {
            return self.send(request, None).await;
        }

        let request = request.build().map_err(|source| Error::from_http(source, None))?;
        let mut remaining = self.submit_max_attempts;
        let mut request_id = None;
        let deadline = tokio::time::Instant::now() + self.timeout;
        let result = tokio::time::timeout_at(deadline, async {
            loop {
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::Timeout { request_id: request_id.clone() });
                }
                remaining -= 1;
                let may_retry = remaining > 0;
                let attempt_number = self.submit_max_attempts - remaining;
                let attempt = request.try_clone()
                    .ok_or(Error::InvalidInput("submission body cannot be replayed"))?;
                let response = match self.http.execute(attempt).await {
                    Ok(response) => response,
                    Err(source) if may_retry && source.is_request() && !source.is_timeout() => {
                        tokio::time::sleep(transport_backoff(attempt_number)).await;
                        continue;
                    }
                    Err(source) => return Err(Error::from_http(source, request_id.clone())),
                };
                if let Some(id) = response_request_id(&response) {
                    request_id = Some(id.to_owned());
                }
                if may_retry && retryable(response.status()) {
                    if let Some(delay) = retry_after(&response) {
                        drop(response);
                        tokio::time::sleep(delay.min(self.timeout)).await;
                        continue;
                    }
                }
                let successful = response.status().is_success();
                match Self::decode(response, request_id.as_deref()).await {
                    Err(Error::Transport { .. }) if successful && may_retry => {
                        tokio::time::sleep(transport_backoff(attempt_number)).await;
                    }
                    result => return result,
                }
            }
        }).await;
        result.map_err(|_| Error::Timeout { request_id })?
    }

    pub async fn get(&self, id: &str) -> Result<RequestTrace, Error> {
        let request = self.request_for(Method::GET, id, "")?;
        self.send(request, Some(id)).await
    }

    pub async fn events(&self, id: &str) -> Result<RequestEvents, Error> {
        let request = self.request_for(Method::GET, id, "/events")?;
        self.send(request, Some(id)).await
    }

    /// Read the request's recorded timing, logs, and provider attempts.
    pub async fn trace(&self, id: &str) -> Result<RequestTraceSnapshot, Error> {
        let request = self.request_for(Method::GET, id, "/trace")?;
        self.send(request, Some(id)).await
    }

    /// Download the request's redacted trace as an OTLP/HTTP JSON document.
    pub async fn trace_otlp(&self, id: &str) -> Result<OtlpTrace, Error> {
        let request = self.request_for(Method::GET, id, "/trace/otlp")?;
        self.send(request, Some(id)).await
    }

    /// Compare request timings with prior successful requests on the same route.
    pub async fn trace_baseline(&self, id: &str) -> Result<TraceBaseline, Error> {
        let request = self.request_for(Method::GET, id, "/trace/baseline")?;
        self.send(request, Some(id)).await
    }

    pub async fn provider_path(&self, id: &str) -> Result<Option<ProviderPath>, Error> {
        #[derive(serde::Deserialize)]
        struct Trace {
            provider_path: Option<ProviderPath>,
        }
        let request = self.request_for(Method::GET, id, "/trace")?;
        let trace: Trace = self.send(request, Some(id)).await?;
        Ok(trace.provider_path)
    }

    pub async fn preview_route(
        &self,
        id: &str,
        options: &RoutePreviewOptions,
    ) -> Result<RoutePreview, Error> {
        let request = self.request_for(Method::POST, id, "/route-preview")?.json(options);
        self.send(request, Some(id)).await
    }

    pub async fn result(&self, id: &str) -> Result<ProviderResult, Error> {
        let request = self.request_for(Method::GET, id, "/result")?;
        self.send(request, Some(id)).await
    }

    pub async fn image_result(&self, id: &str) -> Result<ImageProviderResult, Error> {
        let request = self.request_for(Method::GET, id, "/result")?;
        self.send(request, Some(id)).await
    }

    pub async fn video_result(&self, id: &str) -> Result<VideoProviderResult, Error> {
        let request = self.request_for(Method::GET, id, "/result")?;
        self.send(request, Some(id)).await
    }

    pub async fn audio_result(&self, id: &str) -> Result<AudioProviderResult, Error> {
        let request = self.request_for(Method::GET, id, "/result")?;
        self.send(request, Some(id)).await
    }

    pub async fn mesh_result(&self, id: &str) -> Result<MeshProviderResult, Error> {
        let request = self.request_for(Method::GET, id, "/result")?;
        self.send(request, Some(id)).await
    }

    pub async fn cancel(&self, id: &str) -> Result<RequestTrace, Error> {
        let request = self
            .request_for(Method::POST, id, "/cancel")?
            .json(&serde_json::json!({}));
        self.send(request, Some(id)).await
    }

    pub async fn list(&self, filters: ListOptions<'_>) -> Result<RequestPage, Error> {
        self.list_filtered(RequestFilters {
            client_ref: filters.client_ref,
            after: filters.after,
            ..Default::default()
        }).await
    }

    pub async fn list_filtered(&self, filters: RequestFilters<'_>) -> Result<RequestPage, Error> {
        let request = self
            .request(Method::GET, "v1/requests", true)?
            .query(&filters);
        self.send(request, None).await
    }

    pub async fn list_diagnostic(
        &self,
        filters: RequestFilters<'_>,
        diagnostic: RequestDiagnostic,
    ) -> Result<DiagnosticRequestPage, Error> {
        let request = self
            .request(Method::GET, "v1/requests", true)?
            .query(&filters)
            .query(&[("diagnostic", diagnostic)]);
        self.send(request, None).await
    }

    pub async fn usage(&self, filters: UsageOptions<'_>) -> Result<UsageReport, Error> {
        self.usage_report(filters, None).await
    }

    pub async fn usage_for_app(
        &self,
        app_ref: &str,
        filters: UsageOptions<'_>,
    ) -> Result<UsageReport, Error> {
        self.usage_report(filters, Some(app_ref)).await
    }

    async fn usage_report(
        &self,
        filters: UsageOptions<'_>,
        app_ref: Option<&str>,
    ) -> Result<UsageReport, Error> {
        let request = self.request(Method::GET, "v1/usage", true)?.query(&filters);
        let request = match app_ref {
            Some(reference) => request.query(&[("app_ref", reference)]),
            None => request,
        };
        self.send(request, None).await
    }

    pub async fn wait(&self, id: &str, options: WaitOptions) -> Result<RequestTrace, Error> {
        let request = self.request_for(Method::GET, id, "")?;
        self.poll(request, id, options, |trace: &RequestTrace| {
            Ok(trace.status.is_terminal() || trace.status == RequestStatus::AwaitingReconciliation)
        })
        .await
    }

    pub(super) async fn poll<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        id: &str,
        options: WaitOptions,
        done: impl Fn(&T) -> Result<bool, Error>,
    ) -> Result<T, Error> {
        validate_duration(options.timeout)?;
        validate_duration(options.poll_interval)?;
        tokio::time::timeout(options.timeout, async {
            let mut validator: Option<HeaderValue> = None;
            loop {
                let mut request = request
                    .try_clone()
                    .ok_or(Error::InvalidInput("polling request cannot be replayed"))?;
                if let Some(tag) = &validator {
                    request = request.header(IF_NONE_MATCH, tag.clone());
                }
                let suggested_delay = match Self::poll_once::<T>(request, id, &mut validator).await {
                    Ok((Some(value), _)) if done(&value)? => return Ok(value),
                    Ok((_, delay)) => delay,
                    Err(Error::Transport { .. } | Error::Timeout { .. }) => Duration::ZERO,
                    Err(error) => return Err(error),
                };
                let delay = options.poll_interval.max(suggested_delay);
                tokio::time::sleep(delay.min(options.timeout)).await;
            }
        })
        .await
        .map_err(|_| Error::Timeout {
            request_id: Some(id.to_owned()),
        })?
    }

    async fn poll_once<T: DeserializeOwned>(
        request: RequestBuilder,
        id: &str,
        validator: &mut Option<HeaderValue>,
    ) -> Result<(Option<T>, Duration), Error> {
        let response = request
            .send()
            .await
            .map_err(|source| Error::from_http(source, Some(id.to_owned())))?;
        let status = response.status();
        let suggested_delay = retry_after(&response);
        let unchanged = status == StatusCode::NOT_MODIFIED && validator.is_some();
        let unavailable = retryable(status) && suggested_delay.is_some();
        let delay = suggested_delay.unwrap_or_default();
        if unchanged || unavailable {
            return Ok((None, delay));
        }
        let tag = response.headers().get(ETAG).cloned();
        let value = Self::decode(response, Some(id)).await?;
        *validator = tag;
        Ok((Some(value), delay))
    }

    pub(super) fn request_for(&self, method: Method, id: &str, suffix: &str) -> Result<RequestBuilder, Error> {
        if !valid_id(id, "req_") {
            return Err(Error::InvalidInput("supply a valid TokenCraft request ID"));
        }
        self.request(method, &format!("v1/requests/{id}{suffix}"), true)
    }

    pub(super) fn request(
        &self,
        method: Method,
        path: &str,
        authenticated: bool,
    ) -> Result<RequestBuilder, Error> {
        let url = self
            .origin
            .join(path)
            .map_err(|_| Error::InvalidInput("invalid gateway route"))?;
        let request = self
            .http
            .request(method, url)
            .header(ACCEPT, "application/json");
        if authenticated {
            Ok(request.header(
                AUTHORIZATION,
                self.bearer.as_ref().ok_or(Error::MissingKey)?.clone(),
            ))
        } else {
            Ok(request)
        }
    }

    pub(super) async fn send<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        id_hint: Option<&str>,
    ) -> Result<T, Error> {
        let response = request
            .send()
            .await
            .map_err(|source| Error::from_http(source, id_hint.map(str::to_owned)))?;
        Self::decode(response, id_hint).await
    }

    pub(super) async fn download_verified(
        &self,
        request: RequestBuilder,
        id: &str,
        reference: Option<&ArtifactRef>,
        max_bytes: usize,
    ) -> Result<VerifiedArtifact, Error> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/octet-stream"));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let request = request.headers(headers);

        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut request_id = id.to_owned();
        let mut remaining = self.download_max_attempts;
        let result = tokio::time::timeout_at(deadline, async {
            loop {
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::Timeout { request_id: Some(request_id.clone()) });
                }
                remaining -= 1;
                let may_retry = remaining > 0;
                let attempt_number = self.download_max_attempts - remaining;
                let attempt = request
                    .try_clone()
                    .ok_or(Error::InvalidInput("download request cannot be replayed"))?;
                let response = match attempt.send().await {
                    Ok(response) => response,
                    Err(source) if may_retry && source.is_request() && !source.is_timeout() => {
                        tokio::time::sleep(transport_backoff(attempt_number)).await;
                        continue;
                    }
                    Err(source) => return Err(Error::from_http(source, Some(request_id.clone()))),
                };
                if let Some(id) = response_request_id(&response) {
                    request_id = id.to_owned();
                }
                if may_retry && retryable(response.status()) {
                    if let Some(delay) = retry_after(&response) {
                        drop(response);
                        tokio::time::sleep(delay.min(self.timeout)).await;
                        continue;
                    }
                }
                match verify_download(response, &request_id, reference, max_bytes).await {
                    Err(Error::Transport { .. }) if may_retry => {
                        tokio::time::sleep(transport_backoff(attempt_number)).await;
                    }
                    result => return result,
                }
            }
        })
        .await;
        result.map_err(|_| Error::Timeout { request_id: Some(request_id) })?
    }

    async fn decode<T: DeserializeOwned>(
        response: Response,
        id_hint: Option<&str>,
    ) -> Result<T, Error> {
        let (response, request_id) = Self::checked_response(response, id_hint).await?;
        response.json().await.map_err(|source| Error::from_http(source, request_id))
    }

    pub(super) async fn checked_response(
        response: Response,
        id_hint: Option<&str>,
    ) -> Result<(Response, Option<String>), Error> {
        let request_id = response_request_id(&response).or(id_hint).map(str::to_owned);
        if response.status().is_success() {
            return Ok((response, request_id));
        }
        Err(Error::from_gateway(response, request_id).await)
    }
}

pub(super) fn validate_idempotency_key(key: &str) -> Result<(), Error> {
    if !(8..=128).contains(&key.len())
        || !key.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
        })
    {
        return Err(Error::InvalidInput(
            "use 8 to 128 ASCII letters, numbers or . _ : - in Idempotency-Key",
        ));
    }
    Ok(())
}

fn response_request_id(response: &Response) -> Option<&str> {
    ["tokencraft-request-id", "x-request-id"].into_iter().find_map(|name| {
        response.headers().get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| valid_id(value, "req_"))
    })
}

fn retryable(status: StatusCode) -> bool {
    matches!(status, StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE)
}

fn transport_backoff(attempt_number: u8) -> Duration {
    Duration::from_millis((100 << (attempt_number - 1)).min(1_000))
}

fn retry_after(response: &Response) -> Option<Duration> {
    let mut values = response.headers().get_all(RETRY_AFTER).iter();
    let value = values.next()?.to_str().ok()?.trim();
    if values.next().is_some() || value.is_empty() {
        return None;
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(Duration::from_secs(value.parse().unwrap_or(u64::MAX)));
    }
    httpdate::parse_http_date(value).ok()
        .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
}

fn validate_duration(value: Duration) -> Result<(), Error> {
    if value.is_zero() || Instant::now().checked_add(value).is_none() {
        return Err(Error::InvalidInput(
            "supply a positive, representable timeout or polling interval",
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct QuoteBody<'a> {
    #[serde(flatten)]
    input: &'a QuoteInput,
    #[serde(flatten)]
    options: QuoteOptions<'a>,
}

#[derive(Serialize)]
struct SubmitBody<'a> {
    quote_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pinned_offer_id: Option<&'a str>,
    input: QuoteBody<'a>,
    client_ref: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_ref: Option<&'a str>,
    delivery: Option<Delivery<'a>>,
    messages: Option<&'a [Message<'a>]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_prompt: Option<&'a str>,
    #[serde(flatten)]
    prompts: MediaPrompts<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_options: Option<&'a ChatOptions>,
}

#[derive(Default, Serialize)]
struct MediaPrompts<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    video_prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mesh_prompt: Option<&'a str>,
}

#[derive(Serialize)]
struct Delivery<'a> {
    webhook_subscription_id: &'a str,
}
