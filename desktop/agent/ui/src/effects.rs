use std::time::Duration;

use cosmic::app::Task;
use futures::future::{AbortRegistration, Abortable};

use crate::Message;
use crate::bridge::{
    BridgeEndpoint, ChatRequest, cancel_task, ensure_bridge_endpoint, fetch_history, fetch_models,
    fetch_sessions, session_exists,
};

pub(crate) fn connect_bridge() -> Task<Message> {
    Task::perform(
        async {
            ensure_bridge_endpoint()
                .await
                .map_err(|error| format!("{error:#}"))
        },
        |result| cosmic::Action::App(Message::BridgeConnected(result)),
    )
}

pub(crate) fn fetch_models_task(endpoint: BridgeEndpoint) -> Task<Message> {
    Task::perform(
        async move {
            fetch_models(endpoint)
                .await
                .map_err(|error| format!("{error:#}"))
        },
        |result| cosmic::Action::App(Message::ModelsFetched(result)),
    )
}

pub(crate) fn fetch_sessions_task(endpoint: BridgeEndpoint) -> Task<Message> {
    Task::perform(
        async move {
            fetch_sessions(endpoint)
                .await
                .map_err(|error| format!("{error:#}"))
        },
        |result| cosmic::Action::App(Message::SessionsFetched(result)),
    )
}

pub(crate) fn fetch_history_task(endpoint: BridgeEndpoint, session_id: String) -> Task<Message> {
    Task::perform(
        async move {
            let result = fetch_history(endpoint, &session_id)
                .await
                .map_err(|error| format!("{error:#}"));
            (session_id, result)
        },
        |(session_id, result)| cosmic::Action::App(Message::HistoryFetched { session_id, result }),
    )
}

pub(crate) fn confirm_provisional_task(
    endpoint: BridgeEndpoint,
    session_index: usize,
    session_id: String,
) -> Task<Message> {
    Task::perform(
        async move {
            for attempt in 0..5 {
                match session_exists(endpoint.clone(), &session_id).await {
                    Ok(true) => return (session_id, Ok(true)),
                    Ok(false) if attempt < 4 => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Ok(false) => return (session_id, Ok(false)),
                    Err(error) => return (session_id, Err(format!("{error:#}"))),
                }
            }
            (session_id, Ok(false))
        },
        move |(session_id, result)| {
            cosmic::Action::App(Message::ProvisionalResolved {
                session_index,
                session_id,
                result,
            })
        },
    )
}

pub(crate) fn open_stream(
    endpoint: BridgeEndpoint,
    request: ChatRequest,
    generation: u64,
    abort_registration: AbortRegistration,
) -> Task<Message> {
    cosmic::Task::stream(cosmic::iced::stream::channel(
        32,
        move |mut sender: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            use futures::SinkExt;
            use futures_util::StreamExt;
            let stream_future = async move {
                match crate::sse::open_chat_stream(endpoint, request).await {
                    Ok(stream) => {
                        let mut stream = std::pin::pin!(stream);
                        while let Some(item) = stream.next().await {
                            let message = match item {
                                Ok(event) => Message::Stream(generation, event),
                                Err(error) => {
                                    Message::TransportError(generation, format!("{error:#}"))
                                }
                            };
                            let terminal = matches!(
                                message,
                                Message::Stream(_, crate::bridge::StreamEvent::Error(_))
                                    | Message::TransportError(_, _)
                            );
                            if sender.send(message).await.is_err() || terminal {
                                return;
                            }
                        }
                        let _ = sender.send(Message::StreamEnded(generation)).await;
                    }
                    Err(error) => {
                        let _ = sender
                            .send(Message::TransportError(generation, format!("{error:#}")))
                            .await;
                    }
                }
            };
            let _ = Abortable::new(stream_future, abort_registration).await;
        },
    ))
    .map(cosmic::Action::App)
}

pub(crate) fn cancel_stream(
    endpoint: BridgeEndpoint,
    task_id: String,
    session_index: usize,
    message_index: usize,
) -> Task<Message> {
    Task::perform(
        async move {
            cancel_task(endpoint, &task_id)
                .await
                .map_err(|error| format!("{error:#}"))
        },
        move |result| {
            cosmic::Action::App(Message::CancelFinished {
                session_index,
                message_index,
                result,
            })
        },
    )
}
