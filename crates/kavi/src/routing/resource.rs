use std::panic::Location;

use super::{BuildError, Group, Pattern, PatternError, Route, Router};
use crate::{
    dispatch::{Controller, Middleware},
    http::Method,
};

/// Configures conventional collection and member routes.
///
/// Register actions through [`Router::resource`]. Only supplied actions become
/// routes; there are no default handlers. Names are qualified by this
/// resource and any containing groups, such as `admin.posts.show`.
///
/// | Action | Methods | Path relative to the resource | Name suffix |
/// | --- | --- | --- | --- |
/// | Index | GET | `/` | `index` |
/// | Creation form | GET | `/new` | `new` |
/// | Create | POST | `/` | `create` |
/// | Show | GET | `/{parameter}` | `show` |
/// | Editing form | GET | `/{parameter}/edit` | `edit` |
/// | Update | PUT, PATCH | `/{parameter}` | `update` |
/// | Destroy | DELETE | `/{parameter}` | `destroy` |
///
/// GET supplies implicit HEAD. Group joining maps the relative `/` to the
/// resource base without an added trailing slash. Static paths such as `/new`
/// take precedence over captures within the requested method; they are not
/// reserved identifiers for other methods. No model lookup is performed.
///
/// ```text
/// use kavi::http::{Request, StatusCode};
/// use kavi::routing::{Resource, Router};
///
/// let mut router = Router::new();
/// router.resource(
///     Resource::named("posts")
///         .parameter("post")
///         .parameter_constraint(r"[0-9]+"),
///     |routes| {
///         routes.index(async |_: &mut Request| "Posts");
///         routes.show(async |_: &mut Request| "A post");
///         routes.update(async |_: &mut Request| StatusCode::NO_CONTENT);
///     },
/// );
/// let table = router.build()?;
/// let path = table.path_for("posts.show", &[("post", "42")])?;
/// ```
#[derive(Debug)]
#[must_use]
pub struct Resource {
    name: String,
    path: String,
    parameter: String,
    parameter_constraint: Option<String>,
}

impl Resource {
    /// Creates a resource named `name` at `/{name}`, with member parameter `id`.
    ///
    /// Names are literal and are not singularized. Empty names and invalid path
    /// or parameter declarations fail when the router is built.
    pub fn named(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            path: format!("/{name}"),
            name,
            parameter: "id".to_owned(),
            parameter_constraint: None,
        }
    }

    /// Replaces the absolute path prefix without changing generated route names.
    ///
    /// The prefix follows [`Group`] semantics and may contain ordinary captures,
    /// but cannot contain a catch-all. Outer group prefixes still apply.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Replaces the member capture name without inferring it from the resource name.
    ///
    /// This must name one ordinary capture. It applies to show, edit, update,
    /// and destroy; collection and creation-form routes have no member capture.
    pub fn parameter(mut self, name: impl Into<String>) -> Self {
        self.parameter = name.into();
        self
    }

    /// Sets or replaces the member capture's full-value Rust regex.
    ///
    /// The constraint applies to every member action. An individual [`Route`]
    /// may override it. Invalid expressions fail when the router is built,
    /// including when no member actions are registered.
    pub fn parameter_constraint(mut self, regex: impl Into<String>) -> Self {
        self.parameter_constraint = Some(regex.into());
        self
    }

    pub(super) fn into_group(self, location: &'static Location<'static>) -> Result<Group, BuildError> {
        if self.name.is_empty() {
            return Err(BuildError::EmptyResourceName { location });
        }

        let path = format!("/{{{}}}", self.parameter);
        let validate = || {
            let mut pattern = Pattern::parse(&path)?;
            if pattern.has_catch_all() || pattern.captures.len() != 1 || pattern.captures[0].name != self.parameter {
                return Err(PatternError::InvalidCapture);
            }
            if let Some(regex) = &self.parameter_constraint {
                pattern.set_constraint(&self.parameter, regex)?;
            }
            Ok(())
        };
        validate().map_err(|source| BuildError::Pattern { path, location, source })?;

        Ok(Group::new(self.path).name_prefix(format!("{}.", self.name)))
    }
}

/// Registers selected resource actions against one typed router scope.
///
/// The scope is borrowed only while [`Router::resource`]'s closure runs. Actions
/// return ordinary [`Route`] handles for per-action names, constraints, and
/// middleware. A custom name is relative to the resource's name prefix.
/// Controllers and middleware retain the router's worker-local dependency
/// contract and need not implement `Clone`, `Send`, or `Sync`.
/// Each selected action must be compatible with the router's dependency root.
pub struct ResourceRoutes<'a, S = ()> {
    router: &'a mut Router<S>,
    resource: &'a Resource,
}

impl<'a, S: 'static> ResourceRoutes<'a, S> {
    pub(super) fn from_router(router: &'a mut Router<S>, resource: &'a Resource) -> Self {
        Self { router, resource }
    }

    /// Adds middleware around every action in this resource, in registration order.
    ///
    /// It covers actions registered before or after this call. It does not wrap
    /// routing misses, generated routing errors, or sibling routes.
    pub fn middleware<M: Middleware<S>>(&mut self, middleware: M) -> &mut Self {
        self.router.middleware(middleware);
        self
    }

    /// Registers `GET /` as `index`, relative to the resource prefix.
    #[track_caller]
    pub fn index<C: Controller<S>>(&mut self, controller: C) -> Route<'_, S> {
        self.register([Method::GET], "/", "index", controller, false)
    }

    /// Registers `GET /new` as `new`, for the resource creation form.
    #[allow(
        clippy::new_ret_no_self,
        reason = "The action name is part of the resource routing convention."
    )]
    #[track_caller]
    pub fn new<C: Controller<S>>(&mut self, controller: C) -> Route<'_, S> {
        self.register([Method::GET], "/new", "new", controller, false)
    }

    /// Registers `POST /` as `create`, relative to the resource prefix.
    #[track_caller]
    pub fn create<C: Controller<S>>(&mut self, controller: C) -> Route<'_, S> {
        self.register([Method::POST], "/", "create", controller, false)
    }

    /// Registers `GET /{parameter}` as `show`.
    #[track_caller]
    pub fn show<C: Controller<S>>(&mut self, controller: C) -> Route<'_, S> {
        self.register([Method::GET], self.member_path(), "show", controller, true)
    }

    /// Registers `GET /{parameter}/edit` as `edit`, for a resource editing form.
    #[track_caller]
    pub fn edit<C: Controller<S>>(&mut self, controller: C) -> Route<'_, S> {
        self.register(
            [Method::GET],
            format!("{}/edit", self.member_path()),
            "edit",
            controller,
            true,
        )
    }

    /// Registers `PUT` and `PATCH /{parameter}` as one `update` declaration.
    ///
    /// Both methods share the same owned controller and middleware chain.
    #[track_caller]
    pub fn update<C: Controller<S>>(&mut self, controller: C) -> Route<'_, S> {
        self.register(
            [Method::PUT, Method::PATCH],
            self.member_path(),
            "update",
            controller,
            true,
        )
    }

    /// Registers `DELETE /{parameter}` as `destroy`.
    #[track_caller]
    pub fn destroy<C: Controller<S>>(&mut self, controller: C) -> Route<'_, S> {
        self.register([Method::DELETE], self.member_path(), "destroy", controller, true)
    }

    fn member_path(&self) -> String {
        format!("/{{{}}}", self.resource.parameter)
    }

    #[track_caller]
    fn register(
        &mut self,
        methods: impl IntoIterator<Item = Method>,
        path: impl Into<String>,
        name: &str,
        controller: impl Controller<S>,
        member: bool,
    ) -> Route<'_, S> {
        let mut route = self.router.methods(methods, path, controller);
        route.name(name);
        if member && let Some(regex) = &self.resource.parameter_constraint {
            route.constraint(self.resource.parameter.as_str(), regex.as_str());
        }
        route
    }
}
