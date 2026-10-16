use std::rc::Rc;
use slint::{Model, VecModel};

slint::include_modules!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = AppWindow::new()?;

    let app_weak = app.as_weak();
    app.on_submit_message(move |text| {
        if let Some(app) = app_weak.upgrade() {
            let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
            msgs.push(ChatMsg {
                role: "user".into(),
                author: "You".into(),
                content: text.clone(),
                timestamp: "Just now".into(),
            });
            msgs.push(ChatMsg {
                role: "assistant".into(),
                author: "Luna".into(),
                content: format!("Received: {}", text).into(),
                timestamp: "Just now".into(),
            });
            app.set_transcript(Rc::new(VecModel::from(msgs)).into());
        }
    });

    let app_weak = app.as_weak();
    app.on_select_session(move |id| {
        if let Some(app) = app_weak.upgrade() {
            let sessions: Vec<SessionItem> = app
                .get_sessions()
                .iter()
                .map(|mut s| {
                    s.active = s.id == id;
                    s
                })
                .collect();
            app.set_sessions(Rc::new(VecModel::from(sessions)).into());
        }
    });

    app.run()?;
    Ok(())
}
