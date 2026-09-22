use actix_web::{http::header, web, HttpResponse};
use status_application::publication::Operations;

pub fn operational_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/readyz", web::get().to(ready))
        .route("/diagnostics", web::get().to(diagnostics))
        .route("/metrics", web::get().to(metrics));
}
async fn ready(ops: web::Data<Operations>) -> HttpResponse {
    if ops.ready() {
        HttpResponse::Ok().finish()
    } else {
        HttpResponse::ServiceUnavailable().finish()
    }
}
async fn diagnostics(ops: web::Data<Operations>) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .json(ops.report())
}
async fn metrics(ops: web::Data<Operations>) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .body(ops.metrics())
}
