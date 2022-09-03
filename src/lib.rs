#![allow(clippy::single_component_path_imports)]

#[cfg(feature = "dylink")]
#[allow(unused_imports)]
use dylink;

use deps::*;

pub mod macros;
pub mod user;
pub mod utils;

use crate::utils::*;

use std::future::Future;

pub(crate) use axum::http::StatusCode;
use axum::{
    extract::*,
    response::{self, IntoResponse},
};

pub fn setup_tracing() -> eyre::Result<()> {
    color_eyre::install()?;
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing_subscriber::fmt()
        // .pretty()
        .compact()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .try_init()
        .map_err(|err| eyre::eyre!(err))?;

    Ok(())
}

#[derive(Debug)]
pub struct Config {}

#[derive(Debug)]
pub struct Context {
    pub db_pool: sqlx::postgres::PgPool,
    pub config: Config,
}

pub type SharedContext = std::sync::Arc<Context>;

shadow_rs::shadow!(build);

pub struct ApiDoc;
impl utoipa::OpenApi for ApiDoc {
    fn openapi() -> utoipa::openapi::OpenApi {
        let mut openapi = utoipa::openapi::OpenApiBuilder::new()
            .info(
                utoipa::openapi::InfoBuilder::new()
                    .title(build::PROJECT_NAME)
                    .version(build::PKG_VERSION)
                    .description(Some(format!(
                        r#"{}

Notes:
- Time values are integers despite the `string($date-time)` note they get
                        "#,
                        build::PKG_DESCRIPTION
                    )))
                    .build(),
            )
            .paths({
                let builder = utoipa::openapi::path::PathsBuilder::new();
                let builder = user::paths(builder);
                builder
            })
            .components(Some({
                let builder = utoipa::openapi::ComponentsBuilder::new();
                let builder = user::components(builder);
                builder.build()
            }))
            .tags(Some([user::TAG.into(), DEFAULT_TAG.into()]))
            .build();
        if let Some(components) = openapi.components.as_mut() {
            use utoipa::openapi::security::*;
            components.add_security_scheme(
                "api_key",
                SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("todo_apikey"))),
            )
        }
        openapi
    }
}

#[async_trait::async_trait]
pub trait Endpoint: Clone + Send + Sync + 'static
where
    for<'a> &'a Self::Error: Into<StatusCode>,
{
    type Request: axum::extract::FromRequest<axum::body::Body> + Send + Sync + 'static;
    type Response: serde::Serialize + Send + Sized + 'static;
    type Error: serde::Serialize + Send + Sized + 'static;

    const METHOD: Method;
    const PATH: &'static str;

    async fn handle(
        self,
        ctx: &crate::Context,
        request: Self::Request,
    ) -> Result<Self::Response, Self::Error>;

    fn route(&self) -> axum::Router {
        use utoipa::openapi::PathItemType;
        let wrapper = EndpointWrapper::new(self.clone());
        let method = match Self::METHOD {
            PathItemType::Get => axum::routing::get(wrapper),
            PathItemType::Post => axum::routing::post(wrapper),
            PathItemType::Put => axum::routing::put(wrapper),
            PathItemType::Delete => axum::routing::delete(wrapper),
            PathItemType::Options => axum::routing::options(wrapper),
            PathItemType::Head => axum::routing::head(wrapper),
            PathItemType::Patch => axum::routing::patch(wrapper),
            PathItemType::Trace => axum::routing::trace(wrapper),
            PathItemType::Connect => todo!(),
        };
        axum::Router::new().route(Self::PATH, method)
    }

    fn http(
        &self,
        req: hyper::Request<hyper::Body>,
    ) -> std::pin::Pin<Box<dyn Future<Output = axum::response::Response> + Send>> {
        let this = self.clone();
        Box::pin(async move {
            let mut req_parts = axum::extract::RequestParts::new(req);
            let req = match Self::Request::from_request(&mut req_parts).await {
                Ok(val) => val,
                Err(err) => return err.into_response(),
            };
            let Extension(ctx) =
                match Extension::<crate::SharedContext>::from_request(&mut req_parts).await {
                    Ok(val) => val,
                    Err(err) => return err.into_response(),
                };
            // we have to clone it or the borrow checker biches that &T is
            match this.handle(&ctx, req).await {
                Ok(ok) => response::Json(ok).into_response(),
                Err(err) => (Into::<StatusCode>::into(&err), response::Json(err)).into_response(),
            }
        })
    }
}

/// (statuscode, description, example)
pub type SuccessResponse<Res> = (StatusCode, &'static str, Res);
/// (description, example)
pub type ErrorResponse<Err> = (&'static str, Err);

pub struct Tag {
    name: &'static str,
    desc: &'static str,
}

impl From<Tag> for utoipa::openapi::Tag {
    fn from(tag: Tag) -> Self {
        utoipa::openapi::tag::TagBuilder::new()
            .name(tag.name)
            .description(Some(tag.desc))
            .build()
    }
}

pub const DEFAULT_TAG: Tag = Tag {
    name: "api",
    desc: "This is the catch all tag.",
};

pub fn axum_path_str_to_openapi(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if &s[0..1] == ":" {
                format!("/{{{}}}", &s[1..])
            } else {
                format!("/{s}")
            }
        })
        .collect()
}

#[test]
fn test_axum_path_str_to_openapi() {
    for (expected, path) in [
        ("/users/{id}", "/users/:id"),
        ("/users/{id}/resource/{resID}", "/users/:id/resource/:resID"),
    ] {
        assert_eq!(
            expected,
            &axum_path_str_to_openapi(path)[..],
            "failed on {path}"
        );
    }
}

pub trait DocumentedEndpoint<Req, Res, Err>:
    Endpoint<Request = Req, Response = Res, Error = Err>
where
    Res: utoipa::ToSchema + serde::Serialize + Send + Sized + 'static,
    Err: utoipa::ToSchema + serde::Serialize + Send + Sized + 'static,
    for<'a> &'a Err: Into<StatusCode>,
{
    const TAG: &'static Tag = &DEFAULT_TAG;
    const SUMMARY: &'static str = "";
    const DESCRIPTION: &'static str = "";
    const DEPRECATED: bool = false;

    fn successs() -> Vec<SuccessResponse<Res>> {
        vec![]
    }

    fn errors() -> Vec<ErrorResponse<Err>> {
        vec![]
    }

    fn path_item() -> utoipa::openapi::PathItem {
        let id = <Self as TypeNameRaw>::type_name_raw();
        utoipa::openapi::PathItem::new(
                Self::METHOD,
                utoipa::openapi::path::OperationBuilder::new()
                    .operation_id(Some(id))
                    .deprecated(Some(if Self::DEPRECATED {
                        utoipa::openapi::Deprecated::True
                    } else {
                        utoipa::openapi::Deprecated::False
                    }))
                    .summary(if !Self::SUMMARY.is_empty() {
                        Some(Self::SUMMARY)
                    } else {
                        None
                    })
                    .description(if !Self::DESCRIPTION.is_empty() {
                        Some(Self::DESCRIPTION)
                    } else {
                        None
                    })
                    .tag(Self::TAG.name)
                    .securities(Some([
                        utoipa::openapi::security::SecurityRequirement::new::<
                            &str,
                            [&str; 1usize],
                            &str,
                        >("api_key", [""]),
                    ]))
                    .parameter(
                        utoipa::openapi::path::ParameterBuilder::new()
                            .name("id")
                            .parameter_in(utoipa::openapi::path::ParameterIn::Path)
                            .required(utoipa::openapi::Required::True)
                            .schema(Some(
                                utoipa::openapi::ObjectBuilder::new()
                                    .schema_type(utoipa::openapi::SchemaType::String)
                                    .format(Some(utoipa::openapi::SchemaFormat::Uuid)),
                            )),
                    )
                    .responses({
                        let mut builder = utoipa::openapi::ResponsesBuilder::new();
                        for (code, desc, resp) in &Self::successs() {
                            builder = builder.response(
                                code.to_string(),
                                utoipa::openapi::ResponseBuilder::new()
                                    .description(*desc)
                                    .content(
                                        "application/json",
                                        utoipa::openapi::ContentBuilder::new()
                                            .schema(utoipa::openapi::Ref::from_schema_name(
                                                format!("{id}Response"),
                                            ))
                                            .example(Some(serde_json::to_value(resp).unwrap()))
                                            .build(),
                                    )
                                    .build(),
                            );
                        }
                        for (desc, err) in &Self::errors() {
                            builder = builder.response(
                                Into::<StatusCode>::into(err).to_string(),
                                utoipa::openapi::ResponseBuilder::new()
                                    .description(*desc)
                                    .content(
                                        "application/json",
                                        utoipa::openapi::ContentBuilder::new()
                                            .schema(utoipa::openapi::Ref::from_schema_name(
                                                format!("{id}Error"),
                                            ))
                                            .example(Some(serde_json::to_value(err).unwrap()))
                                            .build(),
                                    )
                                    .build(),
                            );
                        }
                        builder.build()
                    }),
            )
    }

    fn components(
        builder: utoipa::openapi::ComponentsBuilder,
    ) -> utoipa::openapi::ComponentsBuilder {
        let id = <Self as TypeNameRaw>::type_name_raw();
        builder
            .schema(format!("{id}Response"), <Res as utoipa::ToSchema>::schema())
            .schemas_from_iter(<Res as utoipa::ToSchema>::aliases())
            .schema(format!("{id}Error"), <Err as utoipa::ToSchema>::schema())
            .schemas_from_iter(<Err as utoipa::ToSchema>::aliases())
    }
}

pub type Method = utoipa::openapi::PathItemType;

// pub struct DocParameterBuilder {
//     inner: utoipa::openapi::path::ParameterBuilder,
// }
// pub enum ParamExample<T> {
//     Query(T),
//     Path(T),
//     Header(T),
//     Cookie(T),
// }
// impl DocParameterBuilder {
//     pub fn new<T>(name: &'static str, example: ) -> Self {
//         Self {
//             inner: utoipa::openapi::path::ParameterBuilder::new().name(name)
//         }
//     }
//     pub fn build(self: Self) -> Parameter {
//         todo!()
//     }
// }

/// This is used to get around Rust orphaning rules. This allow us
/// to implement any foreign traits lik `axum::handler::Handler` for any `T`
/// that implements `Endpoint`
#[derive(educe::Educe)]
#[educe(Deref, DerefMut)]
pub struct EndpointWrapper<T> {
    inner: T,
}

impl<T, Req, Res, Err> EndpointWrapper<T>
where
    T: Endpoint<Request = Req, Response = Res, Error = Err> + Clone + Send + Sized + 'static,
    Req: axum::extract::FromRequest<axum::body::Body> + Send + Sync + 'static,
    Res: serde::Serialize + Send + Sized + 'static,
    Err: Send + Sized + 'static,
    for<'a> &'a Err: Into<StatusCode>,
{
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T> Clone for EndpointWrapper<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T, Req, Res, Err> axum::handler::Handler<Req> for EndpointWrapper<T>
where
    T: Endpoint<Request = Req, Response = Res, Error = Err> + Clone + Send + Sized + 'static,
    Req: axum::extract::FromRequest<axum::body::Body> + Send + Sync + 'static,
    Res: serde::Serialize + Send + Sized + 'static,
    Err: serde::Serialize + Send + Sized + 'static,
    for<'a> &'a Err: Into<StatusCode>,
{
    type Future = std::pin::Pin<Box<dyn Future<Output = axum::response::Response> + Send>>;

    fn call(self, req: hyper::Request<hyper::Body>) -> Self::Future {
        self.http(req)
    }
}

impl<T, Req, Res, Err> utoipa::Path for EndpointWrapper<T>
where
    T: Endpoint<Request = Req, Response = Res, Error = Err> + DocumentedEndpoint<Req, Res, Err>,
    Req: axum::extract::FromRequest<axum::body::Body> + Send + Sync + 'static,
    Res: utoipa::ToSchema + serde::Serialize + Send + Sized + 'static,
    Err: utoipa::ToSchema + serde::Serialize + Send + Sized + 'static,
    for<'a> &'a Err: Into<StatusCode>,
{
    fn path() -> &'static str {
        <T as Endpoint>::PATH
    }

    fn path_item(_: Option<&str>) -> utoipa::openapi::path::PathItem {
        <T as DocumentedEndpoint<Req, Res, Err>>::path_item()
    }
}