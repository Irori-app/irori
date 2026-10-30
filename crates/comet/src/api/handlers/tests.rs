//! The port is open to every local program. These tests speak to it the way
//! any of them could. `offline_http()` cannot reach the network, so a broken check
//! fails its test without sending anything to GOG.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use protobuf::{Enum, Message};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;

use super::error::MessageHandlingErrorKind;
use super::*;
use crate::api::structs::Token;
use crate::constants::GALAXY_CLIENT_ID;
use crate::proto::galaxy_protocols_communication_service::{
    AuthInfoRequest, CreateLeaderboardRequest, MessageType as Service, SortMethod,
    StartGameSessionRequest, UnlockUserAchievementRequest, UpdateUserStatRequest,
};
use crate::proto::galaxy_protocols_overlay_for_service::{
    AccessTokenResponse, MessageType as OverlayService,
};
use crate::proto::gog_protocols_pb::Header;

const GAME_CLIENT: &str = "53652320436400298";
const ACCESS_TOKEN: &str = "account-access-token";
const REFRESH_TOKEN: &str = "account-refresh-token";

fn offline_http() -> Client {
    Client::builder()
        .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
        .build()
        .unwrap()
}

fn tokens() -> TokenStorage {
    let mut tokens = HashMap::new();
    tokens.insert(
        GALAXY_CLIENT_ID.to_string(),
        Token::new(ACCESS_TOKEN.to_string(), REFRESH_TOKEN.to_string()),
    );
    Arc::new(Mutex::new(tokens))
}

fn allowed(ids: &[&str]) -> AllowedClients {
    let ids: HashSet<String> = ids.iter().map(|id| id.to_string()).collect();
    Arc::new(Mutex::new(ids))
}

fn user() -> Arc<UserInfo> {
    Arc::new(UserInfo {
        username: "player".to_string(),
        galaxy_user_id: "1".to_string(),
    })
}

fn message(sort: u32, type_: u32, payload: Vec<u8>) -> ProtoPayload {
    let mut header = Header::new();
    header.set_sort(sort);
    header.set_type(type_);
    header.set_size(payload.len() as u32);
    ProtoPayload { header, payload }
}

fn sign_in(client_id: &str) -> ProtoPayload {
    let mut request = AuthInfoRequest::new();
    request.set_client_id(client_id.to_string());
    request.set_client_secret("secret".to_string());
    request.set_game_pid(1);
    let payload = request.write_to_bytes().unwrap();
    message(1, Service::AUTH_INFO_REQUEST.value() as u32, payload)
}

fn access_token_request() -> ProtoPayload {
    let type_ = OverlayService::ACCESS_TOKEN_REQUEST.value() as u32;
    message(3, type_, Vec::new())
}

/// A connection the way `entry_point` serves it, with the client's end.
async fn connect(
    allowed: AllowedClients,
) -> (TcpStream, CancellationToken, broadcast::Sender<PusherEvent>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (socket, _) = listener.accept().await.unwrap();
    let (topics, topics_rx) = broadcast::channel(4);
    let (overlay, _) = broadcast::channel(4);
    let stop = CancellationToken::new();
    tokio::spawn(entry_point(
        socket,
        offline_http(),
        tokens(),
        allowed,
        user(),
        topics_rx,
        overlay,
        stop.clone(),
    ));
    (client, stop, topics)
}

async fn context(allowed: AllowedClients) -> (HandlerContext, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (socket, _) = listener.accept().await.unwrap();
    let (overlay, _) = broadcast::channel(4);
    (
        HandlerContext::new(socket, tokens(), allowed, overlay),
        client,
    )
}

async fn send(client: &mut TcpStream, message: &ProtoPayload) {
    let header = message.header.write_to_bytes().unwrap();
    let mut frame = (header.len() as u16).to_be_bytes().to_vec();
    frame.extend(header);
    frame.extend(&message.payload);
    client.write_all(&frame).await.unwrap();
}

/// Everything the service sends until it closes the connection.
async fn read_until_closed(client: &mut TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut received))
        .await
        .expect("the service keeps the connection open")
        .unwrap();
    received
}

async fn handle(
    context: &HandlerContext,
    message: ProtoPayload,
    connection: Connection,
) -> Result<Vec<u8>, MessageHandlingError> {
    handle_message(context, user(), &offline_http(), message, connection).await
}

fn is_unauthorized(result: &Result<Vec<u8>, MessageHandlingError>) -> bool {
    matches!(
        result,
        Err(MessageHandlingError {
            kind: MessageHandlingErrorKind::Unauthorized
        })
    )
}

fn is_invalid(result: &Result<Vec<u8>, MessageHandlingError>) -> bool {
    matches!(
        result,
        Err(MessageHandlingError {
            kind: MessageHandlingErrorKind::Invalid
        })
    )
}

#[tokio::test]
async fn the_port_never_hands_out_the_account_token() {
    let (mut client, stop, _topics) = connect(allowed(&[GAME_CLIENT])).await;
    send(&mut client, &access_token_request()).await;
    let received = read_until_closed(&mut client).await;
    stop.cancel();
    assert!(received.is_empty(), "{received:?}");
}

#[tokio::test]
async fn the_port_refuses_every_overlay_call() {
    let (context, _client) = context(allowed(&[GAME_CLIENT])).await;
    for sort in [3, 7] {
        let result = handle(&context, message(sort, 1, Vec::new()), Connection::Game).await;
        assert!(is_unauthorized(&result), "sort {sort}");
    }
}

#[tokio::test]
async fn the_overlay_pipe_still_answers() {
    let (context, _client) = context(allowed(&[])).await;
    let response = handle(&context, access_token_request(), Connection::Overlay)
        .await
        .unwrap();
    let header_size = u16::from_be_bytes([response[0], response[1]]) as usize;
    let payload = &response[2 + header_size..];
    let token = AccessTokenResponse::parse_from_bytes(payload).unwrap();
    assert_eq!(token.access_token(), ACCESS_TOKEN);
}

#[tokio::test]
async fn galaxy_client_id_cannot_sign_in_even_if_allowed() {
    let (mut client, stop, _topics) = connect(allowed(&[GALAXY_CLIENT_ID])).await;
    send(&mut client, &sign_in(GALAXY_CLIENT_ID)).await;
    let received = read_until_closed(&mut client).await;
    stop.cancel();
    assert!(received.is_empty(), "{received:?}");
}

#[tokio::test]
async fn a_game_that_was_not_started_cannot_sign_in() {
    let (context, _client) = context(allowed(&[GAME_CLIENT])).await;
    let result = handle(&context, sign_in("1207658924"), Connection::Game).await;
    assert!(is_unauthorized(&result));
    assert!(!context.client_identified().await);
}

#[tokio::test]
async fn a_bogus_client_id_is_refused_rather_than_joined_into_a_path() {
    for id in [
        "",
        "../../evil",
        "..",
        "12/34",
        "12 34",
        "١٢٣",
        "1234567890123456789012345678901234",
    ] {
        // Allowed on purpose: the shape is checked whatever the host says.
        let (context, _client) = context(allowed(&[id])).await;
        let result = handle(&context, sign_in(id), Connection::Game).await;
        assert!(is_unauthorized(&result), "{id:?}");
        assert!(!context.client_identified().await, "{id:?}");
    }
}

#[tokio::test]
async fn a_gameplay_database_refuses_ids_that_are_not_numbers() {
    for (client_id, user_id) in [("../evil", "1"), ("1", "../evil"), ("", "1")] {
        assert!(
            crate::db::gameplay::setup_connection(client_id, user_id)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn calls_before_sign_in_are_refused_without_a_panic() {
    let (context, _client) = context(allowed(&[GAME_CLIENT])).await;
    for type_ in [
        Service::GET_USER_STATS_REQUEST,
        Service::GET_USER_ACHIEVEMENTS_REQUEST,
        Service::GET_USER_TIME_PLAYED_REQUEST,
        Service::UNLOCK_USER_ACHIEVEMENT_REQUEST,
        Service::GET_LEADERBOARDS_REQUEST,
    ] {
        let result = handle(
            &context,
            message(1, type_.value() as u32, Vec::new()),
            Connection::Game,
        )
        .await;
        assert!(result.is_err(), "{type_:?}");
    }
    let result = handle(&context, message(6, 1, Vec::new()), Connection::Game).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn a_game_session_starts_without_opening_the_overlay_socket() {
    let (context, _client) = context(allowed(&[])).await;
    let mut overlay_events = context.overlay_sender().subscribe();
    let mut request = StartGameSessionRequest::new();
    request.set_game_pid(42);
    let request = message(
        1,
        Service::START_GAME_SESSION_REQUEST.value() as u32,
        request.write_to_bytes().unwrap(),
    );
    assert!(handle(&context, request, Connection::Game).await.is_ok());
    assert_eq!(context.get_pid().await, 42);
    assert!(overlay_events.try_recv().is_err());
}

/// A client past sign-in, with the real schema on a real SQLite file.
async fn signed_in_context(dir: &std::path::Path) -> (HandlerContext, TcpStream) {
    let (context, client) = context(allowed(&[])).await;
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.join("gameplay.db"))
        .create_if_missing(true);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    sqlx::query(crate::db::gameplay::SETUP_QUERY)
        .execute(&pool)
        .await
        .unwrap();
    context.open_test_database(pool).await;
    context.identify_client(GAME_CLIENT, "secret", 1).await;
    (context, client)
}

fn unlock(achievement_id: u64) -> ProtoPayload {
    let mut request = UnlockUserAchievementRequest::new();
    request.set_achievement_id(achievement_id);
    request.set_time(1_700_000_000);
    message(
        1,
        Service::UNLOCK_USER_ACHIEVEMENT_REQUEST.value() as u32,
        request.write_to_bytes().unwrap(),
    )
}

#[tokio::test]
async fn unlocking_an_unknown_achievement_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let (context, _client) = signed_in_context(dir.path()).await;
    let result = handle(&context, unlock(5), Connection::Game).await;
    assert!(matches!(
        result,
        Err(MessageHandlingError {
            kind: MessageHandlingErrorKind::DB(_)
        })
    ));
}

#[tokio::test]
async fn unlocking_an_achievement_saves_it_for_upload() {
    let dir = tempfile::tempdir().unwrap();
    let (context, _client) = signed_in_context(dir.path()).await;
    sqlx::query(
        "INSERT INTO achievement (id, key, name, description, visible_while_locked,
         image_url_locked, image_url_unlocked, changed) VALUES (5, 'k', 'n', 'd', 1, '', '', 0)",
    )
    .execute(&context.db_connection().await)
    .await
    .unwrap();

    handle(&context, unlock(5), Connection::Game).await.unwrap();

    let achievement = crate::db::gameplay::get_achievement(&context, 5)
        .await
        .unwrap();
    assert!(achievement.date_unlocked().is_some());
    assert!(context.updated_achievements().await);
}

#[tokio::test]
async fn a_message_type_outside_i32_is_only_unknown() {
    let (context, _client) = context(allowed(&[])).await;
    for sort in [1, 2, 6] {
        context.identify_client(GAME_CLIENT, "secret", 1).await;
        let result = handle(
            &context,
            message(sort, u32::MAX, Vec::new()),
            Connection::Game,
        )
        .await;
        assert!(result.is_err(), "sort {sort}");
    }
    for sort in [3, 7] {
        let result = handle(
            &context,
            message(sort, u32::MAX, Vec::new()),
            Connection::Overlay,
        )
        .await;
        assert!(result.is_err(), "sort {sort}");
    }
}

#[tokio::test]
async fn ids_too_large_for_the_database_are_errors() {
    let (context, _client) = context(allowed(&[])).await;
    context.identify_client(GAME_CLIENT, "secret", 1).await;

    let mut stat = UpdateUserStatRequest::new();
    stat.set_stat_id(u64::MAX);
    let stat = message(
        1,
        Service::UPDATE_USER_STAT_REQUEST.value() as u32,
        stat.write_to_bytes().unwrap(),
    );
    assert!(is_invalid(&handle(&context, stat, Connection::Game).await));

    let mut achievement = UnlockUserAchievementRequest::new();
    achievement.set_achievement_id(u64::MAX);
    let achievement = message(
        1,
        Service::UNLOCK_USER_ACHIEVEMENT_REQUEST.value() as u32,
        achievement.write_to_bytes().unwrap(),
    );
    assert!(is_invalid(
        &handle(&context, achievement, Connection::Game).await
    ));
}

#[tokio::test]
async fn a_leaderboard_without_a_sort_method_is_an_error() {
    let (context, _client) = context(allowed(&[])).await;
    context.identify_client(GAME_CLIENT, "secret", 1).await;

    let mut request = CreateLeaderboardRequest::new();
    request.set_sort_method(SortMethod::SORT_METHOD_UNDEFINED);
    let request = message(
        1,
        Service::CREATE_LEADERBOARD_REQUEST.value() as u32,
        request.write_to_bytes().unwrap(),
    );
    assert!(is_invalid(
        &handle(&context, request, Connection::Game).await
    ));
}

#[tokio::test]
async fn an_oversized_message_is_refused_before_its_buffer_exists() {
    let oversized = message(1, 1, Vec::new());
    let mut header = oversized.header;
    header.set_size(u32::MAX);
    let header = header.write_to_bytes().unwrap();
    let size = header.len() as u16;
    let mut stream = header.as_slice();
    assert!(utils::parse_payload(size, &mut stream).await.is_err());
}

#[tokio::test]
async fn topics_wait_for_sign_in() {
    let (context, _client) = context(allowed(&[])).await;
    let topic = "chat".to_string();
    context.subscribe_topic(topic.clone()).await;
    assert!(!context.is_subscribed(&topic).await);
    context.identify_client(GAME_CLIENT, "secret", 1).await;
    assert!(context.is_subscribed(&topic).await);
}
