//! Compile-only illustration of typed paths; no server or runtime is started.

use kavi as framework;
use kavi::{
    Error,
    http::{Request, RequestExt, Response, Uri, response::text},
    routing::{Group, Pattern, Router, TypedPath},
};

#[derive(TypedPath)]
#[typed_path("/users/{id}")]
struct UserPath<T> {
    #[typed_path(constraint = r"(?x)[0-9]+ # numeric identifiers")]
    id: T,
}

#[derive(TypedPath)]
#[typed_path("/files/{*rest}", crate = "framework")]
struct FilePath(String);

#[derive(TypedPath)]
#[typed_path("/café/{{menu}}")]
struct MenuPath;

async fn show_user(request: Request) -> Result<Response, Error> {
    let UserPath { id } = request.extract_head::<UserPath<u64>>().await?;
    Ok(text(format!("User {id}")))
}

async fn show_file(request: Request) -> Result<Response, Error> {
    // The group adds `tenant`, but this field still extracts `rest` by name.
    let FilePath(rest) = request.extract_head::<FilePath>().await?;
    let tenant: String = request.param("tenant")?;
    let captures = request.route_params().len();
    Ok(text(format!("{tenant}: {rest} ({captures} captures)")))
}

async fn show_menu(request: Request) -> Result<Response, Error> {
    let MenuPath = request.extract_head::<MenuPath>().await?;
    Ok(text("Menu"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut router = Router::new();
    router.typed_get::<UserPath<u64>>(show_user).name("users.show");
    router.typed_get::<MenuPath>(show_menu);
    router.group(
        Group::new("/api/{tenant}")
            .constraint("tenant", "[a-z]+")
            .name_prefix("api."),
        |routes| {
            routes.typed_get::<FilePath>(show_file).name("files");
            routes
                .get("/files/{*rest}", show_file)
                .constraint("rest", "images/.*")
                .name("images");
            routes.head("/files/{*rest}", show_file).constraint("rest", "guides/.*");
            routes
                .get("/users/{id}", show_user)
                .constraint("id", "(?x)[0-9]+ # numeric identifiers")
                .name("users");
        },
    );
    router.get("/users/{slug}", show_menu).name("users.slug");
    router.get("/café", show_menu);
    let table = router.build()?;

    let user = UserPath { id: 42_u64 };
    let path = user.to_uri()?;
    let borrowed = UserPath { id: "42" }.to_uri()?;
    let mounted = user.to_uri_with_prefix("/api")?;
    let origin: Uri = "https://example.com".parse()?;
    let url = user.to_url(&origin)?;
    let mounted_url = user.to_url_with_prefix("/api", &origin)?;
    let file = FilePath("guides/getting started.md".into()).to_uri_with_prefix("/api/acme")?;
    let named = table.path_for("api.users", &[("tenant", "acme"), ("id", "42")])?;
    let named_url = table.url_for("api.files", &[("tenant", "acme"), ("rest", "guides/start.md")], &origin)?;
    let standalone = Pattern::parse("/café/{id}")?
        .constraint("id", "[0-9]+")?
        .to_uri(&[("id", "42")])?;
    println!(
        "{} routes: {path}, {borrowed}, {mounted}, {url}, {mounted_url}, {file}, {named}, {named_url}, {standalone}",
        table.routes().len()
    );
    Ok(())
}
