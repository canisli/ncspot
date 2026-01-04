use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;

use librespot_core::authentication::Credentials as RespotCredentials;
use librespot_core::cache::Cache;
use librespot_oauth::OAuthClientBuilder;
use log::{info, trace};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, Scope,
    TokenResponse, TokenUrl, basic::BasicClient,
};
use url::Url;

use crate::config::{self, Config};
use crate::spotify::Spotify;

/// Default Spotify client ID used by ncspot when no custom credentials are configured.
pub const DEFAULT_SPOTIFY_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";

static OAUTH_SCOPES: &[&str] = &[
    "playlist-modify",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read",
    "playlist-read-collaborative",
    "playlist-read-private",
    "streaming",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify",
    "user-modify-playback-state",
    "user-modify-private",
    "user-personalized",
    "user-read-currently-playing",
    "user-read-email",
    "user-read-play-history",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

/// Returns the configured client ID, or the default if not set.
pub fn get_client_id(config: &Config) -> String {
    config
        .values()
        .client_id
        .clone()
        .unwrap_or_else(|| DEFAULT_SPOTIFY_CLIENT_ID.to_string())
}

pub fn find_free_port() -> Result<u16, String> {
    let socket = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    socket
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| e.to_string())
}

pub fn get_client_redirect_uri() -> String {
    let auth_port = find_free_port().expect("Could not find free port");
    format!("http://127.0.0.1:{auth_port}/login")
}

/// Get credentials for use with librespot. This first tries to get cached credentials. If no cached
/// credentials are available it will initiate the OAuth2 login process.
pub fn get_credentials(configuration: &Config) -> Result<RespotCredentials, String> {
    let mut credentials = {
        let cache = Cache::new(Some(config::cache_path("librespot")), None, None, None)
            .expect("Could not create librespot cache");
        let cached_credentials = cache.credentials();
        match cached_credentials {
            Some(c) => {
                info!("Using cached credentials");
                c
            }
            None => {
                info!("Attempting to login via OAuth2");
                credentials_prompt(configuration, None)?
            }
        }
    };

    while let Err(error) = Spotify::test_credentials(configuration, credentials.clone()) {
        let error_msg = format!("{error}");
        credentials = credentials_prompt(configuration, Some(error_msg))?;
    }
    Ok(credentials)
}

fn credentials_prompt(
    config: &Config,
    error_message: Option<String>,
) -> Result<RespotCredentials, String> {
    if let Some(message) = error_message {
        eprintln!("Connection error: {message}");
    }

    create_credentials(config)
}

/// Create credentials using either Authorization Code flow (with client secret)
/// or PKCE flow (without secret).
pub fn create_credentials(config: &Config) -> Result<RespotCredentials, String> {
    println!("To login you need to perform OAuth2 authorization using your web browser\n");

    let client_id = get_client_id(config);
    let client_secret = config.values().client_secret.clone();

    // If both client_id and client_secret are configured, use Authorization Code flow
    if let Some(secret) = client_secret {
        info!("Using Authorization Code flow with client secret");
        create_credentials_with_secret(&client_id, &secret)
    } else {
        info!("Using PKCE flow (no client secret)");
        create_credentials_pkce(&client_id)
    }
}

/// Create credentials using PKCE flow (the default, no client secret required).
fn create_credentials_pkce(client_id: &str) -> Result<RespotCredentials, String> {
    let client_builder = OAuthClientBuilder::new(
        client_id,
        &get_client_redirect_uri(),
        OAUTH_SCOPES.to_vec(),
    );
    let oauth_client = client_builder.build().map_err(|e| e.to_string())?;

    oauth_client
        .get_access_token()
        .map(|token| RespotCredentials::with_access_token(token.access_token))
        .map_err(|e| e.to_string())
}

/// Create credentials using Authorization Code flow with client secret.
fn create_credentials_with_secret(
    client_id: &str,
    client_secret: &str,
) -> Result<RespotCredentials, String> {
    let redirect_uri = get_client_redirect_uri();

    let auth_url = AuthUrl::new("https://accounts.spotify.com/authorize".to_string())
        .map_err(|e| format!("Invalid auth URL: {e}"))?;
    let token_url = TokenUrl::new("https://accounts.spotify.com/api/token".to_string())
        .map_err(|e| format!("Invalid token URL: {e}"))?;
    let redirect_url = RedirectUrl::new(redirect_uri.clone())
        .map_err(|e| format!("Invalid redirect URL: {e}"))?;

    let client = BasicClient::new(ClientId::new(client_id.to_string()))
        .set_client_secret(ClientSecret::new(client_secret.to_string()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url)
        .set_redirect_uri(redirect_url);

    // Build authorization URL with scopes
    let scopes: Vec<Scope> = OAUTH_SCOPES.iter().map(|s| Scope::new(s.to_string())).collect();
    let (auth_url, _csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scopes(scopes)
        .url();

    println!("Browse to: {auth_url}");

    // Open browser automatically
    open::that_in_background(auth_url.as_str());

    // Listen for the callback
    let code = get_authcode_from_redirect(&redirect_uri)?;
    trace!("Received authorization code");

    // Exchange code for token
    let (tx, rx) = mpsc::channel();
    let client_clone = client.clone();
    std::thread::spawn(move || {
        let http_client = reqwest::blocking::Client::new();
        let resp = client_clone.exchange_code(code).request(&http_client);
        let _ = tx.send(resp);
    });

    let token_response = rx.recv().map_err(|_| "Failed to receive token response")?;
    let token = token_response.map_err(|e| format!("Token exchange failed: {e}"))?;

    Ok(RespotCredentials::with_access_token(
        token.access_token().secret().to_string(),
    ))
}

/// Parse the authorization code from the redirect URI.
fn get_code_from_url(redirect_url: &str) -> Result<AuthorizationCode, String> {
    let url = Url::parse(redirect_url).map_err(|e| format!("Failed to parse URL: {e}"))?;
    url.query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, code)| AuthorizationCode::new(code.into_owned()))
        .ok_or_else(|| format!("No authorization code found in URL: {redirect_url}"))
}

/// Get the socket address from a redirect URI if it's HTTP with a port.
fn get_socket_address(redirect_uri: &str) -> Option<SocketAddr> {
    let url = Url::parse(redirect_uri).ok()?;
    if url.scheme() != "http" || url.port().is_none() {
        return None;
    }
    url.socket_addrs(|| None).ok()?.pop()
}

/// Listen for OAuth callback and extract authorization code.
fn get_authcode_from_redirect(redirect_uri: &str) -> Result<AuthorizationCode, String> {
    let socket_address = get_socket_address(redirect_uri)
        .ok_or_else(|| "Could not determine socket address from redirect URI")?;

    let listener = TcpListener::bind(socket_address)
        .map_err(|e| format!("Failed to bind listener: {e}"))?;

    info!("OAuth server listening on {socket_address:?}");

    let mut stream = listener
        .incoming()
        .flatten()
        .next()
        .ok_or("Listener terminated without connection")?;

    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|_| "Failed to read request")?;

    let redirect_path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or("Failed to parse request")?;

    let code = get_code_from_url(&format!("http://localhost{redirect_path}"))?;

    // Send response to browser
    let message = "Authorization successful! You can close this tab and return to ncspot.";
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
        message.len(),
        message
    );
    let _ = stream.write_all(response.as_bytes());

    Ok(code)
}
