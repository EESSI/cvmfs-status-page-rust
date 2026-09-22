//! Actix adapters for registered public artifacts and a separate operations listener.
use actix_web::{
    http::{header, Method},
    web, HttpRequest, HttpResponse,
};
use status_application::publication::{Operations, PublishedSite};
use std::collections::BTreeSet;

/// Construct once outside the Actix application factory.
#[derive(Clone)]
pub struct PublicState {
    site: PublishedSite,
    registered: BTreeSet<String>,
    not_found_page: web::Bytes,
}
impl PublicState {
    pub fn new(
        site: PublishedSite,
        registered: impl IntoIterator<Item = String>,
        not_found_page: String,
    ) -> Self {
        Self {
            site,
            registered: registered.into_iter().collect(),
            not_found_page: web::Bytes::from(not_found_page),
        }
    }
}
pub fn public_routes(cfg: &mut web::ServiceConfig) {
    cfg.default_service(web::to(public));
}
async fn public(request: HttpRequest, state: web::Data<PublicState>) -> HttpResponse {
    let Ok(decoded) = percent_encoding::percent_decode_str(request.path()).decode_utf8() else {
        return not_found(&request, &state);
    };
    let path = if decoded == "/" {
        "index.html"
    } else {
        decoded.strip_prefix('/').unwrap_or(&decoded)
    };
    if !state.registered.contains(path) {
        return not_found(&request, &state);
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return HttpResponse::MethodNotAllowed()
            .insert_header((header::ALLOW, "GET, HEAD"))
            .finish();
    }
    // Only the Arc clone takes a lock. All response work is on immutable memory.
    let Some(bundle) = state.site.current() else {
        return HttpResponse::ServiceUnavailable()
            .insert_header((header::RETRY_AFTER, "1"))
            .insert_header((header::CACHE_CONTROL, "no-store"))
            .finish();
    };
    let Some(artifact) = bundle.get(path) else {
        return not_found(&request, &state);
    };
    let matches = request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|tag| {
                let tag = tag.trim().trim_start_matches("W/");
                tag == "*" || tag == artifact.etag()
            })
        });
    let mut response = if matches {
        HttpResponse::NotModified()
    } else {
        HttpResponse::Ok()
    };
    response
        .insert_header((header::CONTENT_TYPE, artifact.content_type()))
        .insert_header((header::ETAG, artifact.etag()))
        .insert_header((header::CACHE_CONTROL, "public, max-age=0, must-revalidate"));
    if matches {
        response.finish()
    } else if request.method() == Method::HEAD {
        response
            .insert_header((header::CONTENT_LENGTH, artifact.body().len()))
            .body(actix_web::body::SizedStream::new(
                artifact.body().len() as u64,
                futures::stream::empty::<Result<web::Bytes, actix_web::Error>>(),
            ))
    } else {
        response.body(artifact.body().to_vec())
    }
}
fn not_found(request: &HttpRequest, state: &PublicState) -> HttpResponse {
    let mut response = HttpResponse::NotFound();
    response
        .content_type("text/html; charset=utf-8")
        .insert_header((header::CACHE_CONTROL, "no-store"));
    if request.method() == Method::HEAD {
        response
            .insert_header((header::CONTENT_LENGTH, state.not_found_page.len()))
            .body(actix_web::body::SizedStream::new(
                state.not_found_page.len() as u64,
                futures::stream::empty::<Result<web::Bytes, actix_web::Error>>(),
            ))
    } else {
        response.body(state.not_found_page.clone())
    }
}
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

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};
    use status_storage::{Artifact, PublicBundle, PublicPath};
    use std::collections::BTreeMap;
    const NOT_FOUND_PAGE: &str = "<!DOCTYPE html><html><body><h1>Page not found</h1></body></html>";
    #[actix_web::test]
    async fn missing_public_urls_have_visible_errors_and_body_free_head_responses() {
        let site = PublishedSite::default();
        site.publish(
            PublicBundle::new(
                "test".into(),
                1000,
                BTreeMap::from([(
                    PublicPath::new("index.html").unwrap(),
                    Artifact::new("text/html", b"complete".to_vec()).unwrap(),
                )]),
            )
            .unwrap(),
        );
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(PublicState::new(
                    site,
                    ["index.html".into(), "missing.json".into()],
                    NOT_FOUND_PAGE.into(),
                )))
                .configure(public_routes),
        )
        .await;
        for path in ["/does-not-exist", "/config.json", "/%FF", "/missing.json"] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;
            assert_eq!(response.status(), 404, "{path}");
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE).unwrap(),
                "text/html; charset=utf-8"
            );
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL).unwrap(),
                "no-store"
            );
            let body = test::read_body(response).await;
            assert_eq!(body, NOT_FOUND_PAGE);
            let response = test::call_service(
                &app,
                test::TestRequest::default()
                    .method(Method::HEAD)
                    .uri(path)
                    .to_request(),
            )
            .await;
            assert_eq!(response.status(), 404, "{path}");
            assert_eq!(
                response.headers().get(header::CONTENT_LENGTH).unwrap(),
                body.len().to_string().as_str()
            );
            assert!(test::read_body(response).await.is_empty());
        }
    }
    #[actix_web::test]
    async fn cold_routes_are_503_but_private_paths_are_404() {
        let state = web::Data::new(PublicState::new(
            PublishedSite::default(),
            ["index.html".into(), "nested/status.json".into()],
            NOT_FOUND_PAGE.into(),
        ));
        let app = test::init_service(App::new().app_data(state).configure(public_routes)).await;
        for path in ["/", "/index.html", "/nested/status.json"] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;
            assert_eq!(response.status(), 503);
        }
        for path in [
            "/templates/status.html",
            "/config.json",
            "/history/snapshots.jsonl",
            "/committed.json",
            "/generations/x",
            "/%2e%2e/config.json",
        ] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;
            assert_eq!(response.status(), 404);
            assert_eq!(test::read_body(response).await, NOT_FOUND_PAGE);
        }
    }
    #[actix_web::test]
    async fn nested_unicode_and_space_paths_are_decoded_once() {
        let site = PublishedSite::default();
        let path = "pages/Å status.json";
        site.publish(
            PublicBundle::new(
                "test".into(),
                1000,
                BTreeMap::from([(
                    PublicPath::new(path).unwrap(),
                    Artifact::new("application/json", b"{}".to_vec()).unwrap(),
                )]),
            )
            .unwrap(),
        );
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(PublicState::new(
                    site,
                    [path.into()],
                    NOT_FOUND_PAGE.into(),
                )))
                .configure(public_routes),
        )
        .await;
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/pages/%C3%85%20status.json")
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(test::read_body(response).await, b"{}"[..]);
    }
    #[actix_web::test]
    async fn head_and_revalidation_use_the_same_immutable_representation() {
        let site = PublishedSite::default();
        site.publish(
            PublicBundle::new(
                "test".into(),
                1000,
                BTreeMap::from([(
                    PublicPath::new("index.html").unwrap(),
                    Artifact::new("text/html", b"complete".to_vec()).unwrap(),
                )]),
            )
            .unwrap(),
        );
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(PublicState::new(
                    site,
                    ["index.html".into()],
                    NOT_FOUND_PAGE.into(),
                )))
                .configure(public_routes),
        )
        .await;
        let response =
            test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
        let etag = response.headers().get(header::ETAG).unwrap().clone();
        assert_eq!(test::read_body(response).await, b"complete"[..]);
        let response = test::call_service(
            &app,
            test::TestRequest::default()
                .method(Method::HEAD)
                .uri("/index.html")
                .to_request(),
        )
        .await;
        assert_eq!(response.headers().get(header::CONTENT_LENGTH).unwrap(), "8");
        assert!(test::read_body(response).await.is_empty());
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/")
                .insert_header((header::IF_NONE_MATCH, etag))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), 304);
        assert!(test::read_body(response).await.is_empty());
    }
}
