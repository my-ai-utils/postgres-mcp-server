use std::sync::Arc;

use my_http_server::controllers::ControllersMiddleware;

use crate::app::AppContext;

pub fn build(app: &Arc<AppContext>) -> ControllersMiddleware {
    let mut result = ControllersMiddleware::new(None, None);

    result.register_get_action(Arc::new(super::settings_controller::GetSettingsAction::new(
        app.clone(),
    )));

    result.register_post_action(Arc::new(
        super::settings_controller::SetMcpWritesAction::new(app.clone()),
    ));

    result.register_get_action(Arc::new(super::requests_controller::GetRequestsAction::new(
        app.clone(),
    )));

    result
}
