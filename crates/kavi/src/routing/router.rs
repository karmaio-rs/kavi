use std::{collections::HashMap, panic::Location, rc::Rc};

use super::{
    BuildError, Group, Pattern, Route, RouteInfo, RoutingTable, TypedPath,
    matcher::{Candidate, InsertError, Matcher, MatcherBuilder},
    route::{DeclarationPattern, RouteDeclaration},
    table::Endpoint,
};
use crate::{
    dispatch::{
        Controller, ControllerFactory, ErasedMiddleware, Middleware, MiddlewareFactory, defer_controller,
        defer_middleware,
    },
    http::Method,
};

/// Collects routes, groups, and a root fallback for validation at startup.
///
/// Method registration returns a [`Route`] handle for naming and middleware.
/// Groups inherit path and name prefixes and middleware. Merged routers retain
/// their middleware scopes. [`Self::build_with_dependencies`] binds declarations
/// to one application-owned context and compiles an immutable [`RoutingTable`].
/// Router middleware covers its entire scope, regardless of when routes are
/// registered. At the root it also wraps fallbacks, misses, and routing errors.
/// Controllers and middleware need not be `Send` or `Sync`.
/// `S` is the dependency root required by the declarations, checked at registration.
/// The router does not own that root until compilation. Injection borrows services
/// or explicitly clones annotated owned parameters, without placing them in the
/// HTTP request. Construct runtime-bound services on their worker and retain the
/// context for application lifecycle management.
///
/// Patterns support: `/users/{id}`, `/{file}.json`, and
/// `/files/{*rest}`. Literal braces are escaped as `{{` and `}}`. Recognition
/// uses the requested method first, then radix static/parameter/catch-all
/// specificity within that method. Constrained captures precede unconstrained
/// candidates of the same shape; equal candidates use registration order.
/// Unicode literals are encoded in both declaration styles. [`TypedPath`] supports
/// typed registration without changing the controller's request contract.
#[must_use]
pub struct Router<S = ()> {
    scope: Scope<S>,
}

struct Scope<S> {
    declarations: Vec<Declaration<S>>,
    middleware: Vec<MiddlewareFactory<S>>,
}

impl<S> Default for Scope<S> {
    #[inline]
    fn default() -> Self {
        Self {
            declarations: Vec::new(),
            middleware: Vec::new(),
        }
    }
}

enum Declaration<S> {
    Route(RouteDeclaration<S>),
    Group(Group, Scope<S>, &'static Location<'static>),
    Merge(Scope<S>),
    Fallback(ControllerFactory<S>, &'static Location<'static>),
}

impl<S: 'static> Default for Router<S> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    /// Compiles declarations that require no application dependencies.
    ///
    /// # Errors
    /// Returns [`BuildError`] for invalid or conflicting declarations.
    #[inline]
    pub fn build(self) -> Result<RoutingTable, BuildError> {
        self.build_with_dependencies(Rc::new(()))
    }
}

impl<S: 'static> Router<S> {
    /// Creates empty route declarations requiring application dependencies of type `S`.
    #[inline]
    pub fn new() -> Self {
        Self {
            scope: Scope::default(),
        }
    }

    /// Adds middleware to this router's whole scope in registration order.
    ///
    /// At the root, middleware wraps every outcome, including the fallback and
    /// routing errors, before group, merged-router, and route middleware. Within a
    /// group closure, it wraps selected routes before nested scopes and route
    /// middleware. A merged router's middleware covers its imported endpoints,
    /// including its fallback, without wrapping siblings or general routing errors.
    /// It applies to routes registered before or after this call. All chains are
    /// compiled by [`Self::build_with_dependencies`].
    pub fn middleware<M: Middleware<S>>(&mut self, middleware: M) -> &mut Self {
        self.scope.middleware.push(defer_middleware(middleware));
        self
    }

    /// Registers a controller for an exact, case-sensitive HTTP method.
    ///
    /// Extension methods are supported. Invalid declarations fail at build time.
    #[track_caller]
    pub fn on<C: Controller<S>>(&mut self, method: Method, path: impl Into<String>, controller: C) -> Route<'_, S> {
        self.methods([method], path, controller)
    }

    /// Registers several methods against one owned controller and middleware chain.
    ///
    /// An empty list, repeated methods, or duplicate registrations fail at build time.
    #[track_caller]
    pub fn methods<C: Controller<S>>(
        &mut self,
        methods: impl IntoIterator<Item = Method>,
        path: impl Into<String>,
        controller: C,
    ) -> Route<'_, S> {
        self.register(methods, DeclarationPattern::Text(path.into()), controller)
    }

    #[track_caller]
    fn register(
        &mut self,
        methods: impl IntoIterator<Item = Method>,
        pattern: DeclarationPattern,
        controller: impl Controller<S>,
    ) -> Route<'_, S> {
        self.scope.declarations.push(Declaration::Route(RouteDeclaration {
            pattern,
            constraints: Vec::new(),
            methods: methods.into_iter().collect(),
            controller: defer_controller(controller),
            middleware: Vec::new(),
            name: None,
            location: Location::caller(),
        }));
        let Some(Declaration::Route(declaration)) = self.scope.declarations.last_mut() else {
            unreachable!("the declaration just appended is a route");
        };
        Route { declaration }
    }

    /// Registers an exact HTTP method using a typed path's declared pattern.
    ///
    /// The controller still takes one owned request. Extract the path value
    /// inside the controller through [`crate::http::RequestExt::extract_head`].
    #[track_caller]
    pub fn typed_on<P: TypedPath>(&mut self, method: Method, controller: impl Controller<S>) -> Route<'_, S> {
        self.typed_methods::<P>([method], controller)
    }

    /// Registers several methods using one typed path and owned controller.
    ///
    /// Group prefixes apply during compilation. URL generation from `P` remains
    /// independent and requires an explicit prefix for mounted paths.
    #[track_caller]
    pub fn typed_methods<P: TypedPath>(
        &mut self,
        methods: impl IntoIterator<Item = Method>,
        controller: impl Controller<S>,
    ) -> Route<'_, S> {
        self.register(methods, DeclarationPattern::Typed(P::pattern().clone()), controller)
    }

    /// Registers routes in a group with inherited path, name, and middleware configuration.
    ///
    /// The closure runs immediately. Group configuration applies to every route
    /// within it. A child `/` denotes the group base without a trailing slash.
    /// A single slash joins each boundary; other slashes remain significant.
    /// Register middleware on the child router inside the closure.
    #[track_caller]
    pub fn group(&mut self, group: Group, configure: impl FnOnce(&mut Self)) -> &mut Self {
        let mut router = Self::new();
        configure(&mut router);
        self.scope
            .declarations
            .push(Declaration::Group(group, router.scope, Location::caller()));
        self
    }

    /// Imports another router's declarations while preserving its middleware scope.
    ///
    /// Declaration order is preserved. Parent prefixes, constraints, and middleware
    /// apply to imported routes. Imported middleware wraps only those endpoints,
    /// including an imported root fallback, and runs after parent middleware.
    /// Both routers require the same dependency type and bind to the same owner
    /// when built. Merging a fallback into a group or registering multiple root
    /// fallbacks fails at build time. General routing errors use only the outer
    /// router's middleware.
    pub fn merge(&mut self, other: Self) -> &mut Self {
        self.scope.declarations.push(Declaration::Merge(other.scope));
        self
    }

    /// Registers an unnamed root fallback for paths unmatched by every method.
    ///
    /// It runs through table middleware, including for non-path request targets.
    /// It does not handle unsupported methods, malformed captures, or controller errors.
    /// Multiple fallbacks or a fallback within a group fail at build time.
    #[track_caller]
    pub fn fallback<C: Controller<S>>(&mut self, controller: C) -> &mut Self {
        self.scope
            .declarations
            .push(Declaration::Fallback(defer_controller(controller), Location::caller()));
        self
    }

    /// Binds one shared application context and compiles validated routing declarations.
    ///
    /// The context need not implement `Clone`, `Send`, or `Sync`. Every controller
    /// and middleware projects from the same owner, retained by bound endpoints.
    /// This includes all groups, merged scopes, route middleware, and the fallback.
    /// Borrowed bindings do not clone their targets; owned macro bindings clone
    /// only when explicitly annotated. Keep an application-owned `Rc` clone for
    /// jobs, tasks, and asynchronous shutdown.
    /// The resulting table erases the dependency type and does not initialize or
    /// shut down services.
    ///
    /// GET supplies implicit HEAD, with explicit HEAD preferred for the same
    /// shape. Names are globally unique, including on identical paths.
    /// Structurally equivalent patterns may use different capture names and
    /// constraints. Identical selection conditions conflict within one method.
    ///
    /// # Errors
    /// Returns [`BuildError`] for invalid or conflicting declarations, with their
    /// registration locations. A failed build consumes all declarations and does
    /// not expose a partially compiled table.
    pub fn build_with_dependencies(self, dependencies: Rc<S>) -> Result<RoutingTable, BuildError> {
        let mut compiler = Compiler::default();
        let table_chain = compiler.inherit(&[], self.scope.middleware, &dependencies);
        compiler.compile(
            self.scope.declarations,
            &Pattern::parse("/").expect("static root pattern"),
            "",
            &table_chain,
            true,
            &dependencies,
        )?;

        let paths = compile_methods(&compiler.routes)?;

        Ok(RoutingTable {
            paths,
            names: compiler.names,
            routes: compiler.routes,
            endpoints: compiler.endpoints,
            middleware: compiler.middleware,
            table_chain,
            fallback: compiler.fallback.map(|(endpoint, _)| endpoint),
        })
    }
}

macro_rules! methods {
    ($($name:ident, $typed:ident => $method:ident),+ $(,)?) => {$(
        impl<S: 'static> Router<S> {
            #[doc = concat!("Registers a controller for `", stringify!($method), "` requests.")]
            #[track_caller]
            pub fn $name<C: Controller<S>>(&mut self, path: impl Into<String>, controller: C) -> Route<'_, S> {
                self.on(Method::$method, path, controller)
            }

            #[doc = concat!("Registers a controller for `", stringify!($method), "` using a typed path's pattern.")]
            #[track_caller]
            pub fn $typed<P: TypedPath>(&mut self, controller: impl Controller<S>) -> Route<'_, S> {
                self.typed_on::<P>(Method::$method, controller)
            }
        }
    )+};
}

methods! {
    get, typed_get => GET,
    post, typed_post => POST,
    put, typed_put => PUT,
    patch, typed_patch => PATCH,
    delete, typed_delete => DELETE,
    head, typed_head => HEAD,
    options, typed_options => OPTIONS,
    connect, typed_connect => CONNECT,
    trace, typed_trace => TRACE,
}

#[derive(Default)]
struct Compiler {
    routes: Vec<RouteInfo>,
    endpoints: Vec<Endpoint>,
    names: HashMap<String, usize>,
    middleware: Vec<Box<dyn ErasedMiddleware>>,
    fallback: Option<(Endpoint, &'static Location<'static>)>,
}

impl Compiler {
    fn compile<S: 'static>(
        &mut self,
        declarations: Vec<Declaration<S>>,
        prefix: &Pattern,
        name_prefix: &str,
        inherited: &[usize],
        root: bool,
        dependencies: &Rc<S>,
    ) -> Result<(), BuildError> {
        for declaration in declarations {
            match declaration {
                Declaration::Route(route) => {
                    let mut pattern = match route.pattern {
                        DeclarationPattern::Text(path) => {
                            Pattern::parse(&path).map_err(|source| BuildError::Pattern {
                                path,
                                location: route.location,
                                source,
                            })?
                        }
                        DeclarationPattern::Typed(pattern) => pattern,
                    };
                    pattern.prepend(prefix).map_err(|source| BuildError::Pattern {
                        path: format!("{} + {}", prefix.as_str(), pattern.as_str()),
                        location: route.location,
                        source,
                    })?;
                    for (name, regex) in route.constraints {
                        pattern
                            .set_constraint(&name, &regex)
                            .map_err(|source| BuildError::Pattern {
                                path: pattern.as_str().to_owned(),
                                location: route.location,
                                source,
                            })?;
                    }
                    if route.methods.is_empty() {
                        return Err(BuildError::EmptyRoute {
                            path: pattern.as_str().to_owned(),
                            location: route.location,
                        });
                    }

                    for (i, method) in route.methods.iter().enumerate() {
                        if route.methods[..i].contains(method) {
                            return Err(BuildError::DuplicateMethod {
                                path: pattern.as_str().to_owned(),
                                method: method.clone(),
                                first: route.location,
                                second: route.location,
                            });
                        }
                    }

                    let name = route.name.map(|name| format!("{name_prefix}{name}"));
                    if let Some(name) = &name {
                        if name.is_empty() {
                            return Err(BuildError::EmptyName {
                                path: pattern.as_str().to_owned(),
                                location: route.location,
                            });
                        }
                        if let Some(&id) = self.names.get(name) {
                            return Err(BuildError::DuplicateName {
                                name: name.clone(),
                                first: self.routes[id].location,
                                second: route.location,
                            });
                        }
                        self.names.insert(name.clone(), self.routes.len());
                    }

                    let chain = self.inherit(inherited, route.middleware, dependencies);

                    self.routes.push(RouteInfo {
                        pattern,
                        methods: route.methods,
                        name,
                        location: route.location,
                    });

                    self.endpoints.push(Endpoint {
                        controller: (route.controller)(Rc::clone(dependencies)),
                        chain,
                    });
                }
                Declaration::Group(group, scope, location) => {
                    let mut group_prefix = Pattern::parse(&group.prefix).map_err(|source| BuildError::Pattern {
                        path: group.prefix,
                        location,
                        source,
                    })?;
                    group_prefix.prepend(prefix).map_err(|source| BuildError::Pattern {
                        path: format!("{} + {}", prefix.as_str(), group_prefix.as_str()),
                        location,
                        source,
                    })?;
                    if group_prefix.has_catch_all() {
                        return Err(BuildError::CatchAllPrefix {
                            path: group_prefix.as_str().to_owned(),
                            location,
                        });
                    }
                    for (name, regex) in group.constraints {
                        group_prefix
                            .set_constraint(&name, &regex)
                            .map_err(|source| BuildError::Pattern {
                                path: group_prefix.as_str().to_owned(),
                                location,
                                source,
                            })?;
                    }
                    let name_prefix = format!("{name_prefix}{}", group.name_prefix);
                    let chain = self.inherit(inherited, scope.middleware, dependencies);
                    self.compile(
                        scope.declarations,
                        &group_prefix,
                        &name_prefix,
                        &chain,
                        false,
                        dependencies,
                    )?;
                }
                Declaration::Merge(scope) => {
                    let chain = self.inherit(inherited, scope.middleware, dependencies);
                    self.compile(scope.declarations, prefix, name_prefix, &chain, root, dependencies)?;
                }
                Declaration::Fallback(controller, location) => {
                    if !root {
                        return Err(BuildError::NestedFallback { location });
                    }
                    if let Some((_, first)) = &self.fallback {
                        return Err(BuildError::DuplicateFallback {
                            first,
                            second: location,
                        });
                    }
                    self.fallback = Some((
                        Endpoint {
                            controller: controller(Rc::clone(dependencies)),
                            chain: inherited.to_vec(),
                        },
                        location,
                    ));
                }
            }
        }
        Ok(())
    }

    fn inherit<S: 'static>(
        &mut self,
        inherited: &[usize],
        middleware: Vec<MiddlewareFactory<S>>,
        dependencies: &Rc<S>,
    ) -> Vec<usize> {
        let mut chain = Vec::with_capacity(inherited.len() + middleware.len());
        chain.extend_from_slice(inherited);
        chain.extend(self.middleware.len()..self.middleware.len() + middleware.len());
        self.middleware
            .extend(middleware.into_iter().map(|bind| bind(Rc::clone(dependencies))));

        chain
    }
}

fn compile_methods(routes: &[RouteInfo]) -> Result<HashMap<Method, Matcher>, BuildError> {
    let tokens: Vec<_> = routes.iter().map(|route| route.pattern.tokens()).collect();
    let mut methods = HashMap::<Method, Vec<Candidate>>::new();
    for (id, route) in routes.iter().enumerate() {
        for method in &route.methods {
            methods
                .entry(method.clone())
                .or_default()
                .push(Candidate { id, implicit: false });
        }
        if route.methods.contains(&Method::GET) && !route.methods.contains(&Method::HEAD) {
            methods
                .entry(Method::HEAD)
                .or_default()
                .push(Candidate { id, implicit: true });
        }
    }
    let mut methods: Vec<_> = methods.into_iter().collect();
    methods.sort_unstable_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
    let mut indexes = HashMap::new();
    for (method, candidates) in methods {
        let mut paths = MatcherBuilder::default();
        for candidate in candidates {
            let route = &routes[candidate.id];
            if let Err(error) = paths.insert(&tokens[candidate.id], candidate, routes) {
                let (id, duplicate) = match error {
                    InsertError::Conflict(id) => (id, false),
                    InsertError::Duplicate(id) => (id, true),
                };
                let path = route.path().to_owned();
                let first = routes[id].location;
                let second = route.location;
                return Err(if duplicate {
                    BuildError::DuplicateMethod {
                        path,
                        method,
                        first,
                        second,
                    }
                } else {
                    BuildError::Conflict {
                        path,
                        method,
                        first,
                        second,
                    }
                });
            }
        }
        indexes.insert(method, paths.finish(routes));
    }
    Ok(indexes)
}
