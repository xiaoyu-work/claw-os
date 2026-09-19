use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use super::backend::Operation;
use super::events::{turn_error, turn_view};
use super::history;
use super::input;
use super::pagination;
use super::protocol::{
    backend_string, notification, Params, RpcError, MAX_HISTORY_ROWS, MAX_MESSAGE_BYTES,
};
use super::server::{Connection, Outcome};

const PAGE_BYTES: usize = MAX_MESSAGE_BYTES * 3 / 4;

impl Connection {
    pub async fn start_thread(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, input::THREAD_FIELDS)?;
        input::settings(&params, self.service.backend.info())?;
        params.reject_present(&[
            "threadId",
            "lastTurnId",
            "beforeTurnId",
            "initialTurnsPage",
            "excludeTurns",
        ])?;
        let _creation = self.service.creation.lock().await;
        self.service.can_load().await?;
        let conversation = history::conversation(
            self.service
                .backend
                .call(Operation::ConversationCreate, json!({}))
                .await?,
        )?;
        if conversation
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| !messages.is_empty())
            || conversation
                .get("jobs")
                .and_then(Value::as_array)
                .is_some_and(|jobs| !jobs.is_empty())
        {
            return Err(RpcError::backend(
                "conversation.create returned existing history instead of a new conversation",
            ));
        }
        let canonical = backend_string(&conversation, "id")?.to_string();
        let thread = self.service.loaded(&canonical).await?;
        self.configure_model(&thread, &params, None).await?;
        self.subscribe(&canonical).await;
        let mut view = history::thread_view(
            &conversation,
            self.service.backend.info(),
            true,
            None,
            Vec::new(),
        )?;
        view["model"] = json!(self.current_model(&thread).await);
        Ok(Outcome {
            result: history::started_response(view.clone(), self.service.backend.info()),
            notifications: vec![notification("thread/started", json!({ "thread": view }))],
            drive: None,
        })
    }

    pub async fn resume_thread(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, input::THREAD_FIELDS)?;
        input::settings(&params, self.service.backend.info())?;
        params.reject_present(&[
            "historyMode",
            "sessionStartSource",
            "lastTurnId",
            "beforeTurnId",
            "threadSource",
        ])?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let thread = self.service.loaded(&canonical).await?;
        let _operation = thread.operation.lock().await;
        let conversation = self.service.conversation(&canonical).await?;
        let jobs = history::jobs(self.service.backend.as_ref(), &conversation).await?;
        self.configure_model(&thread, &params, Some(&jobs)).await?;
        let active = self.service.active(&thread, &conversation).await?;
        *thread.active.lock().await = active
            .as_ref()
            .map(|job| backend_string(job, "id").map(str::to_string))
            .transpose()?;
        let exclude_turns = params.boolean("excludeTurns", false)?;
        if !exclude_turns || params.value("initialTurnsPage").is_some() {
            history::verify_visible_history(&conversation, &jobs)?;
        }
        let turns = if exclude_turns {
            Vec::new()
        } else {
            self.full_history(&conversation, &jobs).await?
        };
        let mut view = history::thread_view(
            &conversation,
            self.service.backend.info(),
            true,
            active.as_ref(),
            turns,
        )?;
        view["model"] = json!(self.current_model(&thread).await);
        let mut response = history::started_response(view, self.service.backend.info());
        response["initialTurnsPage"] = match params.value("initialTurnsPage") {
            Some(page) => {
                let page = Params::new(page.clone(), &["limit", "sortDirection", "itemsView"])?;
                self.turns_page(&conversation, &jobs, &page).await?
            }
            None => Value::Null,
        };
        response["turnsBackwardsCursor"] = Value::Null;
        response["itemsBackwardsCursor"] = Value::Null;
        let drive = self.resume_drive(&canonical, active).await?;
        Ok(Outcome {
            result: response,
            notifications: Vec::new(),
            drive,
        })
    }

    pub async fn read_thread(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, &["threadId", "includeTurns"])?;
        let include_turns = params.boolean("includeTurns", false)?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let conversation = self.service.conversation(&canonical).await?;
        let jobs = history::jobs(self.service.backend.as_ref(), &conversation).await?;
        let thread = self.service.loaded(&canonical).await?;
        self.configure_model(&thread, &params, Some(&jobs)).await?;
        let active = self.service.active(&thread, &conversation).await?;
        let loaded = self
            .subscriptions
            .lock()
            .await
            .get(&canonical)
            .is_some_and(|subscription| subscription.load(Ordering::Acquire));
        if include_turns {
            history::verify_visible_history(&conversation, &jobs)?;
        }
        let turns = if include_turns {
            self.full_history(&conversation, &jobs).await?
        } else {
            Vec::new()
        };
        let mut view = history::thread_view(
            &conversation,
            self.service.backend.info(),
            loaded,
            active.as_ref(),
            turns,
        )?;
        view["model"] = json!(self.current_model(&thread).await);
        Ok(Outcome::result(json!({ "thread": view })))
    }

    pub async fn list_threads(&self, value: Value) -> Result<Outcome, RpcError> {
        let scope = pagination::scope("threads", &value);
        let params = Params::new(
            value,
            &[
                "cursor",
                "limit",
                "sortKey",
                "sortDirection",
                "modelProviders",
                "sourceKinds",
                "originators",
                "archived",
                "sectionId",
                "projectId",
                "cwd",
                "useStateDbOnly",
                "searchTerm",
                "parentThreadId",
                "ancestorThreadId",
            ],
        )?;
        let limit = params.limit(25)?;
        let descending = input::descending(&params, true)?;
        let sort_key = params.string("sortKey")?.unwrap_or("created_at");
        if !matches!(sort_key, "created_at" | "updated_at" | "recency_at") {
            return Err(RpcError::option("sortKey"));
        }
        params.reject_present(&[
            "sectionId",
            "projectId",
            "parentThreadId",
            "ancestorThreadId",
        ])?;
        params.reject_nonempty_array("originators")?;
        params.boolean("useStateDbOnly", false)?;
        let providers = params.unique_strings("modelProviders", 32)?;
        let sources = params.unique_strings("sourceKinds", 32)?;
        let cwd_matches = match params.value("cwd") {
            None => true,
            Some(Value::String(cwd)) => {
                std::path::Path::new(cwd) == self.service.backend.info().home
            }
            Some(Value::Array(cwds)) => {
                if cwds.len() > 32 || cwds.iter().any(|cwd| !cwd.is_string()) {
                    return Err(RpcError::params("Invalid cwd filter"));
                }
                cwds.iter().any(|cwd| {
                    cwd.as_str().is_some_and(|cwd| {
                        std::path::Path::new(cwd) == self.service.backend.info().home
                    })
                })
            }
            _ => return Err(RpcError::params("cwd must be a path or path list")),
        };
        let archived = params.boolean("archived", false)?;
        let search = params.string("searchTerm")?.map(str::to_lowercase);
        let result = self
            .service
            .backend
            .call(
                Operation::ConversationList,
                json!({ "limit": MAX_HISTORY_ROWS, "archived": archived }),
            )
            .await?;
        let conversations = result
            .get("conversations")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcError::backend("Claw returned no conversation list"))?;
        if conversations.len() >= MAX_HISTORY_ROWS as usize
            || result
                .get("conversations_truncated")
                .and_then(Value::as_bool)
                == Some(true)
        {
            return Err(RpcError::unavailable(
                "Claw conversation listing needs a paginated backend beyond its bounded view",
            ));
        }
        let mut views = Vec::new();
        for conversation in conversations {
            if !cwd_matches
                || (!providers.is_empty() && !providers.contains("claw"))
                || (!sources.is_empty() && !sources.contains("cli"))
                || search.as_ref().is_some_and(|search| {
                    !conversation
                        .get("title")
                        .and_then(Value::as_str)
                        .is_some_and(|title| title.to_lowercase().contains(search))
                })
            {
                continue;
            }
            let conversation = self
                .service
                .with_parent_identity(conversation.clone())
                .await?;
            let mut view = history::thread_view(
                &conversation,
                self.service.backend.info(),
                false,
                None,
                Vec::new(),
            )?;
            view["model"] = json!(
                self.service
                    .loaded_model(backend_string(&conversation, "id")?)
                    .await
            );
            views.push(view);
        }
        let field = if sort_key == "created_at" {
            "createdAt"
        } else {
            "updatedAt"
        };
        views.sort_by(|left, right| {
            left[field]
                .as_i64()
                .cmp(&right[field].as_i64())
                .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
        });
        if descending {
            views.reverse();
        }
        let ids = views
            .iter()
            .map(|view| backend_string(view, "id").map(str::to_string))
            .collect::<Result<Vec<_>, _>>()?;
        let start = pagination::start(&params, &scope, &ids)?;
        let end = start.saturating_add(limit).min(views.len());
        let data = views[start..end].to_vec();
        let next = if end < views.len() && end > start {
            pagination::encode(&scope, &ids[end - 1], false)
        } else {
            Value::Null
        };
        let backwards = ids
            .get(start)
            .filter(|_| start < end)
            .map_or(Value::Null, |id| pagination::encode(&scope, id, true));
        Ok(Outcome::result(
            json!({ "data": data, "nextCursor": next, "backwardsCursor": backwards }),
        ))
    }

    pub async fn loaded_threads(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, &["cursor", "limit"])?;
        let limit = params.limit(64)?;
        let subscriptions = self.subscriptions.lock().await;
        let canonical_ids = subscriptions
            .iter()
            .filter(|(_, subscription)| subscription.load(Ordering::Acquire))
            .map(|(canonical, _)| canonical.clone())
            .collect::<Vec<_>>();
        drop(subscriptions);
        let mut ids = Vec::with_capacity(canonical_ids.len());
        for canonical in canonical_ids {
            ids.push(self.service.loaded(&canonical).await?.frontend_id.clone());
        }
        ids.sort();
        let start = pagination::start(&params, "loaded", &ids)?;
        let end = start.saturating_add(limit).min(ids.len());
        let next = if end < ids.len() && end > start {
            pagination::encode("loaded", &ids[end - 1], false)
        } else {
            Value::Null
        };
        Ok(Outcome::result(
            json!({ "data": ids[start..end], "nextCursor": next }),
        ))
    }

    pub async fn unsubscribe_thread(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, &["threadId"])?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        self.service.conversation(&canonical).await?;
        let subscriptions = self.subscriptions.lock().await;
        let status = match subscriptions.get(&canonical) {
            Some(subscription) if subscription.swap(false, Ordering::AcqRel) => "unsubscribed",
            Some(_) => "notSubscribed",
            None => "notLoaded",
        };
        Ok(Outcome::result(json!({ "status": status })))
    }

    pub async fn update_thread(&self, method: &str, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(
            value,
            if method == "thread/name/set" {
                &["threadId", "name"]
            } else {
                &["threadId"]
            },
        )?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let mut update = json!({ "id": canonical });
        let event = match method {
            "thread/name/set" => {
                let name = params.required_string("name")?;
                if name.trim().is_empty()
                    || name.chars().count() > 256
                    || name.chars().any(char::is_control)
                {
                    return Err(RpcError::params(
                        "Thread name must be a nonempty title of at most 256 characters",
                    ));
                }
                update["title"] = json!(name);
                "thread/name/updated"
            }
            "thread/archive" => {
                update["archived"] = json!(true);
                "thread/archived"
            }
            "thread/unarchive" => {
                update["archived"] = json!(false);
                "thread/unarchived"
            }
            "thread/delete" => {
                update["deleted"] = json!(true);
                "thread/deleted"
            }
            _ => return Err(RpcError::unsupported(method)),
        };
        let thread = self.service.loaded(&canonical).await?;
        let _operation = thread.operation.lock().await;
        let conversation = self.service.conversation(&canonical).await?;
        if method != "thread/name/set"
            && self.service.active(&thread, &conversation).await?.is_some()
        {
            return Err(RpcError::busy());
        }
        let result = self
            .service
            .backend
            .call(Operation::ConversationUpdate, update)
            .await?;
        let returned = result
            .get("conversation")
            .ok_or_else(|| RpcError::backend("Claw update has no conversation"))?;
        if backend_string(returned, "id")? != canonical
            || history::frontend_id(returned)? != thread.frontend_id
        {
            return Err(RpcError::backend("Claw updated a different conversation"));
        }
        let mut event_params = json!({ "threadId": thread.frontend_id });
        if method == "thread/name/set" {
            event_params["threadName"] = json!(super::protocol::safe_text(backend_string(
                returned, "title"
            )?));
        }
        let response = if method == "thread/unarchive" {
            let returned = self.service.with_parent_identity(returned.clone()).await?;
            let mut view = history::thread_view(
                &returned,
                self.service.backend.info(),
                true,
                None,
                Vec::new(),
            )?;
            view["model"] = json!(self.current_model(&thread).await);
            json!({ "thread": view })
        } else {
            json!({})
        };
        Ok(Outcome {
            result: response,
            notifications: vec![notification(event, event_params)],
            drive: None,
        })
    }

    pub async fn fork_thread(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, input::THREAD_FIELDS)?;
        input::settings(&params, self.service.backend.info())?;
        params.reject_present(&["historyMode", "sessionStartSource", "initialTurnsPage"])?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let before = params.string("beforeTurnId")?;
        let last = params.string("lastTurnId")?;
        if before.is_some() && last.is_some() {
            return Err(RpcError::params(
                "beforeTurnId and lastTurnId are mutually exclusive",
            ));
        }
        let exclude_turns = params.boolean("excludeTurns", false)?;
        let source = self.service.loaded(&canonical).await?;
        let _operation = source.operation.lock().await;
        let _creation = self.service.creation.lock().await;
        self.service.can_load().await?;
        let conversation = self.service.conversation(&canonical).await?;
        if self.service.active(&source, &conversation).await?.is_some() {
            return Err(RpcError::busy());
        }
        let mut request = json!({ "id": canonical });
        let source_jobs = history::jobs(self.service.backend.as_ref(), &conversation).await?;
        history::verify_visible_history(&conversation, &source_jobs)?;
        if let Some(boundary) = before.or(last) {
            let (start, end, _) =
                history::task_user_turn_range(&conversation, &source_jobs, boundary)?;
            request["before_user_turn"] = json!(if last.is_some() { end } else { start });
            request["expected_revision"] = json!(history::mutation_revision(&conversation)?);
        }
        let fork = history::conversation(
            self.service
                .backend
                .call(Operation::ConversationFork, request)
                .await?,
        )?;
        let fork = self.service.with_parent_identity(fork).await?;
        let fork_id = backend_string(&fork, "id")?.to_string();
        let fork_state = self.service.loaded(&fork_id).await?;
        let model = match params.string("model")? {
            Some(model) => model.to_string(),
            None => self.current_model(&source).await,
        };
        *fork_state.model.lock().await = Some(model.clone());
        self.subscribe(&fork_id).await;
        let turns = if exclude_turns {
            Vec::new()
        } else {
            let jobs = history::jobs(self.service.backend.as_ref(), &fork).await?;
            self.full_history(&fork, &jobs).await?
        };
        let mut thread =
            history::thread_view(&fork, self.service.backend.info(), true, None, turns)?;
        thread["model"] = json!(model);
        Ok(Outcome {
            result: history::started_response(thread.clone(), self.service.backend.info()),
            notifications: vec![notification("thread/started", json!({ "thread": thread }))],
            drive: None,
        })
    }

    pub async fn revert_thread(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, &["threadId", "beforeTurnId"])?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let boundary = params.required_string("beforeTurnId")?;
        let thread = self.service.loaded(&canonical).await?;
        let _operation = thread.operation.lock().await;
        let conversation = self.service.conversation(&canonical).await?;
        if self.service.active(&thread, &conversation).await?.is_some() {
            return Err(RpcError::busy());
        }
        let jobs = history::jobs(self.service.backend.as_ref(), &conversation).await?;
        history::verify_visible_history(&conversation, &jobs)?;
        let (index, _, total) = history::task_user_turn_range(&conversation, &jobs, boundary)?;
        let revision = history::mutation_revision(&conversation)?;
        let updated = history::conversation(
            self.service
                .backend
                .call(
                    Operation::ConversationRevert,
                    json!({ "id": canonical, "user_turns": total - index, "expected_revision": revision }),
                )
                .await?,
        )?;
        let updated = self.service.with_parent_identity(updated).await?;
        if backend_string(&updated, "id")? != canonical
            || history::frontend_id(&updated)? != thread.frontend_id
        {
            return Err(RpcError::backend("Claw reverted a different conversation"));
        }
        let mut view = history::thread_view(
            &updated,
            self.service.backend.info(),
            true,
            None,
            Vec::new(),
        )?;
        view["model"] = json!(self.current_model(&thread).await);
        Ok(Outcome {
            result: json!({ "thread": view, "turnsBackwardsCursor": null, "itemsBackwardsCursor": null }),
            notifications: vec![notification(
                "thread/reverted",
                json!({ "threadId": thread.frontend_id }),
            )],
            drive: None,
        })
    }

    pub async fn list_turns(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(
            value,
            &["threadId", "cursor", "limit", "sortDirection", "itemsView"],
        )?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let conversation = self.service.conversation(&canonical).await?;
        let jobs = history::jobs(self.service.backend.as_ref(), &conversation).await?;
        history::verify_visible_history(&conversation, &jobs)?;
        self.turns_page(&conversation, &jobs, &params)
            .await
            .map(Outcome::result)
    }

    async fn turns_page(
        &self,
        conversation: &Value,
        jobs: &[Value],
        params: &Params,
    ) -> Result<Value, RpcError> {
        let descending = input::descending(params, true)?;
        let canonical = backend_string(conversation, "id")?;
        let frontend_id = history::frontend_id(conversation)?;
        let limit = params.limit(25)?;
        let items_view = input::items_view(params, "summary")?;
        let scope = format!("turns:{canonical}");
        let ordered: Vec<_> = if descending {
            jobs.iter().rev().collect()
        } else {
            jobs.iter().collect()
        };
        let ids = ordered
            .iter()
            .map(|job| backend_string(job, "id").map(str::to_string))
            .collect::<Result<Vec<_>, _>>()?;
        let start = pagination::start(params, &scope, &ids)?;
        let mut turns = Vec::new();
        let mut bytes = 0usize;
        let mut end = start;
        for job in ordered.iter().skip(start).take(limit) {
            let turn = if items_view == "notLoaded" {
                metadata_turn(job)?
            } else {
                history::hydrate(self.service.backend.as_ref(), frontend_id, job, &items_view)
                    .await?
            };
            let size = turn.to_string().len();
            if bytes.saturating_add(size) > PAGE_BYTES {
                if turns.is_empty() {
                    return Err(RpcError::capacity());
                }
                break;
            }
            bytes += size;
            end += 1;
            turns.push(turn);
        }
        Ok(json!({
            "data": turns,
            "nextCursor": if end < ids.len() && end > start {
                pagination::encode(&scope, &ids[end - 1], false)
            } else { Value::Null },
            "backwardsCursor": ids.get(start).filter(|_| start < end)
                .map_or(Value::Null, |id| pagination::encode(&scope, id, true)),
        }))
    }

    pub async fn list_items(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(
            value,
            &["threadId", "turnId", "cursor", "limit", "sortDirection"],
        )?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let turn_filter = params.string("turnId")?;
        let limit = params.limit(100)?;
        let descending = input::descending(&params, false)?;
        let scope = format!("items:{canonical}:{}", turn_filter.unwrap_or(""));
        let conversation = self.service.conversation(&canonical).await?;
        let mut jobs = history::jobs(self.service.backend.as_ref(), &conversation).await?;
        history::verify_visible_history(&conversation, &jobs)?;
        if let Some(turn) = turn_filter {
            jobs.retain(|job| job["id"].as_str() == Some(turn));
            if jobs.is_empty() {
                return Err(RpcError::stale(
                    "turnId does not belong to this conversation",
                ));
            }
        }
        if descending {
            jobs.reverse();
        }
        let anchor = pagination::decode(&params, &scope)?;
        let start_job = if let Some(anchor) = &anchor {
            let (job, _) = anchor
                .id
                .split_once('\n')
                .ok_or_else(|| RpcError::params("Invalid item cursor"))?;
            jobs.iter()
                .position(|candidate| candidate["id"].as_str() == Some(job))
                .ok_or_else(|| RpcError::stale("Item cursor task no longer exists"))?
        } else {
            0
        };
        let mut data = Vec::new();
        let mut keys = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        'jobs: for (job_index, job) in jobs.iter().enumerate().skip(start_job) {
            let job_id = backend_string(job, "id")?;
            let turn = history::hydrate(
                self.service.backend.as_ref(),
                history::frontend_id(&conversation)?,
                job,
                "full",
            )
            .await?;
            let mut items = turn
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| RpcError::backend("Hydrated turn has no items"))?
                .clone();
            if descending {
                items.reverse();
            }
            let start_item = if job_index == start_job {
                if let Some(anchor) = &anchor {
                    let (_, item) = anchor
                        .id
                        .split_once('\n')
                        .ok_or_else(|| RpcError::params("Invalid item cursor"))?;
                    items
                        .iter()
                        .position(|candidate| candidate["id"].as_str() == Some(item))
                        .ok_or_else(|| RpcError::stale("Item pagination anchor no longer exists"))?
                        + usize::from(!anchor.inclusive)
                } else {
                    0
                }
            } else {
                0
            };
            for item in items.into_iter().skip(start_item) {
                let key = format!("{job_id}\n{}", backend_string(&item, "id")?);
                let entry = json!({ "turnId": job_id, "item": item });
                let size = entry.to_string().len();
                if data.len() >= limit || bytes.saturating_add(size) > PAGE_BYTES {
                    if data.is_empty() {
                        return Err(RpcError::capacity());
                    }
                    more = true;
                    break 'jobs;
                }
                bytes += size;
                data.push(entry);
                keys.push(key);
            }
        }
        Ok(Outcome::result(json!({
            "data": data,
            "nextCursor": keys.last().filter(|_| more)
                .map_or(Value::Null, |key| pagination::encode(&scope, key, false)),
            "backwardsCursor": keys.first()
                .map_or(Value::Null, |key| pagination::encode(&scope, key, true)),
        })))
    }

    async fn full_history(
        &self,
        conversation: &Value,
        jobs: &[Value],
    ) -> Result<Vec<Value>, RpcError> {
        if jobs.len() > 100 {
            return Err(RpcError::option(
                "full history (use excludeTurns and thread/turns/list)",
            ));
        }
        let mut turns = Vec::new();
        let mut bytes = 0usize;
        for job in jobs {
            let turn = history::hydrate(
                self.service.backend.as_ref(),
                history::frontend_id(conversation)?,
                job,
                "full",
            )
            .await?;
            bytes = bytes.saturating_add(turn.to_string().len());
            if bytes > PAGE_BYTES {
                return Err(RpcError::option(
                    "full history (use bounded thread/items/list pages)",
                ));
            }
            turns.push(turn);
        }
        Ok(turns)
    }
}

fn metadata_turn(job: &Value) -> Result<Value, RpcError> {
    let (status, error) = match backend_string(job, "status")? {
        "pending" | "running" | "waiting_approval" => ("inProgress", None),
        "ok" => ("completed", None),
        "cancelled" => ("interrupted", None),
        "error" => (
            "failed",
            Some(turn_error(
                job.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Claw task failed"),
            )),
        ),
        _ => return Err(RpcError::backend("Unknown Claw task status")),
    };
    let mut turn = turn_view(backend_string(job, "id")?, Vec::new(), status, error, job);
    turn["itemsView"] = json!("notLoaded");
    Ok(turn)
}
