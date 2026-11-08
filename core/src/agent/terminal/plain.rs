//! Plain chat uses the same durable Root task path, not an unscoped local loop.

use super::{
    backend::{Backend, BrokerBackend, StreamError},
    state::{App, EntryKind},
    ChatOptions,
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, IsTerminal, Write},
    sync::Arc,
    time::Duration,
};

pub(super) fn run(options: ChatOptions) -> Result<Value, String> {
    if unsafe { libc::geteuid() } == 0 {
        return Err(crate::agentd::spawn::ROOT_OWNER_REFUSAL.into());
    }
    let config = crate::config::current_snapshot();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move{
        let backend=Arc::new(BrokerBackend::new(config,unsafe{libc::geteuid()}).await?);
        let conversation=match options.session_id.as_deref(){
            Some(id)=>{
                let conversation=backend.get_conversation(id).await?;
                if options.workspace_id.as_ref().is_some_and(|space|conversation.workspace_id.as_ref()!=Some(space)){
                    return Err("Workspace mismatch; switching context requires another conversation".into());
                }
                conversation
            }
            None=>backend.create_conversation_in_workspace(options.workspace_id.as_deref()).await?,
        };
        let mut app=App::new(backend.info().clone(),conversation);
        let streaming=!options.no_stream;
        let stdout_tty=std::io::stdout().is_terminal();
        eprintln!("cos agent chat — provider={} model={} session={} space={} memory={} streaming={}",
            app.info.provider,app.selected_model,app.conversation.id,app.conversation.workspace_id.as_deref().unwrap_or("personal"),
            !options.no_memory,streaming);
        eprintln!("/new /space UUID|none /workspace PATH|home /history [N] /session /quit");
        let mut prompts=0usize;
        loop{
            if std::io::stdin().is_terminal(){eprint!("> ");let _=std::io::stderr().flush();}
            let line=tokio::task::spawn_blocking(||{
                let mut line=String::new();
                std::io::stdin().lock().read_line(&mut line).map(|n|(n,line))
            }).await.map_err(|e|e.to_string())?.map_err(|e|e.to_string())?;
            if line.0==0{break;}
            let prompt=line.1.trim().to_string();
            if prompt.is_empty(){continue;}
            if matches!(prompt.as_str(),"/quit"|"/exit"){break;}
            if prompt=="/help"{eprintln!("/new /space UUID|none /workspace PATH|home /history [N] /session /quit");continue;}
            if prompt=="/session"{eprintln!("session={} space={} cwd={}",app.conversation.id,
                app.conversation.workspace_id.as_deref().unwrap_or("personal"),app.selected_workspace);continue;}
            if prompt=="/new"{
                let conversation=backend.create_conversation_in_workspace(app.conversation.workspace_id.as_deref()).await?;
                app.replace_conversation(conversation);continue;
            }
            if let Some(space)=prompt.strip_prefix("/space "){
                let space=if space=="none"{None}else{crate::workspaces::validate_id(space)?;Some(space)};
                let conversation=backend.create_conversation_in_workspace(space).await?;
                app.replace_conversation(conversation);continue;
            }
            if let Some(path)=prompt.strip_prefix("/workspace "){
                app.selected_workspace=backend.resolve_workspace((path!="home").then_some(path)).await?;continue;
            }
            if prompt=="/history" || prompt.starts_with("/history "){
                let count=prompt.strip_prefix("/history ").map(str::parse::<usize>).transpose().map_err(|e|e.to_string())?.unwrap_or(20).min(200);
                let conversation=backend.get_conversation(&app.conversation.id).await?;
                for message in conversation.messages.iter().rev().take(count).rev(){eprintln!("{}: {}",message.role,message.text);}
                continue;
            }
            if !app.info.provider_ready{return Err("Configure a text model before submitting work; Workspace management remains available through cos workspace.".into());}
            let job=backend.submit(&prompt,&[],&app.conversation.id,&app.selected_workspace,None,!options.no_memory,
                options.max_turns,&app.selected_model,None,false).await?;
            app.begin_task(&job);
            let mut cursor=0u64;
            let mut retries=0;
            let mut emitted=0usize;
            let finished=loop{
                let frame=match backend.stream(&job.id,cursor).await{
                    Ok(frame)=>{retries=0;frame}
                    Err(StreamError::Retryable(error)) if retries<60=>{
                        retries+=1;eprintln!("Reconnecting to durable task {}: {error}",job.id);
                        tokio::time::sleep(Duration::from_secs(1)).await;continue;
                    }
                    Err(StreamError::Retryable(error)|StreamError::Fatal(error))=>return Err(format!("Task {} remains inspectable; stream unavailable: {error}",job.id)),
                };
                cursor=frame.cursor;
                for record in frame.records{
                    if record.get("progress").and_then(|p|p.get("kind")).and_then(Value::as_str)==Some("context_read"){continue;}
                    let previous=app.entries.len();
                    super::presentation::apply_record(&mut app,&record)?;
                    if options.show_tools{
                        for entry in &app.entries[previous..]{
                            if matches!(entry.kind,EntryKind::Tool{..}){eprintln!("{}",entry.text);}
                        }
                    }
                    if streaming{
                        let answer=app.entries.iter().filter(|entry|entry.kind==EntryKind::Assistant)
                            .map(|entry|entry.text.as_str()).collect::<String>();
                        if answer.len()>=emitted{eprint!("{}",&answer[emitted..]);let _=std::io::stderr().flush();emitted=answer.len();}
                    }
                }
                if frame.terminal{break frame.job;}
            };
            app.finish_task(&finished);
            if streaming{eprintln!();}
            match finished.status.as_str(){
                "ok"=>{if !streaming || !stdout_tty{println!("{}",finished.response.as_deref().unwrap_or(""));}}
                _=>eprintln!("Task {} {}: {}",finished.id,finished.status,finished.error.as_deref().unwrap_or("no final answer")),
            }
            prompts+=1;
            app.clear_transcript();
        }
        Ok(json!({"session_id":app.conversation.id,"workspace_id":app.conversation.workspace_id,"prompts":prompts,"backend":"clawd"}))
    })
}
