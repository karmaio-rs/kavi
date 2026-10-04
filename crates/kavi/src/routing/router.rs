use std::{collections::HashMap, panic::Location};

use super::{
    BuildError, Group, Pattern, Route, RouteInfo, RoutingTable, TypedPath,
    matcher::{Candidate, InsertError, Matcher, MatcherBuilder},
    route::DeclarationPattern,
    table::Endpoint,
};
use crate::{
    dispatch::{Controller, ErasedController, ErasedMiddleware, Middleware},
    http::Method,
};

/// Collects routes, groups, and a root fallback for validation at startup.
///
/// Method registration returns a borrowed [`Route`] for naming and middleware.
/// Groups inherit path and name prefixes and middleware; merges move ownership.
/// [`Self::build`] consumes the declarations into an immutable [`RoutingTable`].
/// Router middleware covers its entire scope, regardless of when routes are
/// registered. At the root it also wraps fallbacks, misses, and routing errors.
/// Controllers and middleware need not be `Send` or `Sync`.
///
/// Patterns support: `/users/{id}`, `/{file}.json`, and
/// `/files/{*rest}`. Literal braces are escaped as `{{` and `}}`. Recognition
/// uses the requested method first, then radix static/parameter/catch-all
/// specificity within that method. Constrained captures precede unconstrained
/// candidates of the same shape; equal candidates use registration order.
/// Unicode literals are encoded in both declaration styles. [`TypedPath`] supports
/// typed registration without changing the controller's request contract.
#[derive(Default)]
#[must_use]
pub struct Router {
    declarations: Vec<Declaration>,
    middleware: Vec<Box<dyn ErasedMiddleware>>,
}

enum Declaration {
    Route(Route),
    Group(Group, Router, &'static Location<'static>),
    Fallback(Box<dyn ErasedController>, &'static Location<'static>),
}

impl Router {
    /// Creates an empty router.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds middleware to this router's whole scope in registration order.
    ///
    /// At the root, middleware wraps every outcome, including the fallback and
    /// routing errors, before group and route middleware. Within a group closure,
    /// it wraps selected routes after the group's configured middleware and before
    /// nested group and route middleware. It applies to routes registered before
    /// or after this call. All chains are compiled by [`Self::build`].
    pub fn middleware<M: Middleware>(&mut self, middleware: M) -> &mut Self {
        self.middleware.push(Box::new(middleware));
        self
    }

    /// Registers a controller for an exact, case-sensitive HTTP method.
    ///
    /// Extension methods are supported. Invalid declarations fail at build time.
    #[track_caller]
    pub fn on<C: Controller>(&mut self, method: Method, path: impl Into<String>, controller: C) -> &mut Route {
        self.methods([method], path, controller)
    }

    /// Registers several methods against one owned controller and middleware chain.
    ///
    /// An empty list, repeated methods, or duplicate registrations fail at build time.
    #[track_caller]
    pub fn methods<C: Controller>(
        &mut self,
        methods: impl IntoIterator<Item = Method>,
        path: impl Into<String>,
        controller: C,
    ) -> &mut Route {
        self.register(methods, DeclarationPattern::Text(path.into()), controller)
    }

    #[track_caller]
    fn register(
        &mut self,
        methods: impl IntoIterator<Item = Method>,
        pattern: DeclarationPattern,
        controller: impl Controller,
    ) -> &mut Route {
        self.declarations.push(Declaration::Route(Route {
            pattern,
            constraints: Vec::new(),
            methods: methods.into_iter().collect(),
            controller: Box::new(controller),
            middleware: Vec::new(),
            name: None,
            location: Location::caller(),
        }));
        let Some(Declaration::Route(route)) = self.declarations.last_mut() else {
            unreachable!("the declaration just appended is a route");
        };
        route
    }

    /// Registers an exact HTTP method using a typed path's declared pattern.
    ///
    /// The controller still takes one owned request. Extract the path value
    /// inside the controller through [`crate::http::RequestExt::extract_head`].
    #[track_caller]
    pub fn typed_on<P: TypedPath>(&mut self, method: Method, controller: impl Controller) -> &mut Route {
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
        controller: impl Controller,
    ) -> &mut Route {
        self.register(methods, DeclarationPattern::Typed(P::pattern().clone()), controller)
    }

    /// Registers routes in a group with inherited path, name, and middleware configuration.
    ///
    /// The closure runs immediately. Group configuration applies to every route
    /// within it. A child `/` denotes the group base without a trailing slash.
    /// A single slash joins each boundary; other slashes remain significant.
    #[track_caller]
    pub fn group(&mut self, group: Group, configure: impl FnOnce(&mut Router)) -> &mut Self {
        let mut router = Self::new();
        configure(&mut router);
        self.declarations
            .push(Declaration::Group(group, router, Location::caller()));
        self
    }

    /// Moves another router's declarations and middleware into the current scope.
    ///
    /// Declaration order is preserved. Middleware is appended in registration
    /// order and applies to the whole receiving scope, including existing routes.
    /// Group inheritance also applies to merged declarations. Merging a fallback
    /// into a group or registering multiple root fallbacks fails at build time.
    pub fn merge(&mut self, other: Self) -> &mut Self {
        self.declarations.extend(other.declarations);
        self.middleware.extend(other.middleware);
        self
    }

    /// Registers an unnamed root fallback for paths unmatched by every method.
    ///
    /// It runs through table middleware, including for non-path request targets.
    /// It does not handle unsupported methods, malformed captures, or controller errors.
    /// Multiple fallbacks or a fallback within a group fail at build time.
    #[track_caller]
    pub fn fallback<C: Controller>(&mut self, controller: C) -> &mut Self {
        self.declarations
            .push(Declaration::Fallback(Box::new(controller), Location::caller()));
        self
    }

    /// Validates declarations and compiles method indexes, dispatch chains, and route metadata.
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
    pub fn build(self) -> Result<RoutingTable, BuildError> {
        let mut compiler = Compiler::default();
        let table_chain = compiler.inherit(&[], self.middleware);
        compiler.compile(
            self.declarations,
            &Pattern::parse("/").expect("static root pattern"),
            "",
            &table_chain,
            true,
        )?;

        let paths = compile_methods(&compiler.routes)?;

        Ok(RoutingTable {
            paths,
            names: compiler.names,
            routes: compiler.routes,
            endpoints: compiler.endpoints,
            middleware: compiler.middleware,
            table_chain,
            fallback: compiler.fallback.map(|(controller, _)| controller),
        })
    }
}

macro_rules! methods {
    ($($name:ident, $typed:ident => $method:ident),+ $(,)?) => {$(
        impl Router {
            #[doc = concat!("Registers a controller for `", stringify!($method), "` requests.")]
            #[track_caller]
            pub fn $name<C: Controller>(&mut self, path: impl Into<String>, controller: C) -> &mut Route {
                self.on(Method::$method, path, controller)
            }

            #[doc = concat!("Registers a controller for `", stringify!($method), "` using a typed path's pattern.")]
            #[track_caller]
            pub fn $typed<P: TypedPath>(&mut self, controller: impl Controller) -> &mut Route {
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
    fallback: Option<(Box<dyn ErasedController>, &'static Location<'static>)>,
}

impl Compiler {
    fn compile(
        &mut self,
        declarations: Vec<Declaration>,
        prefix: &Pattern,
        name_prefix: &str,
        inherited: &[usize],
        root: bool,
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

                    let chain = self.inherit(inherited, route.middleware);

                    self.routes.push(RouteInfo {
                        pattern,
                        methods: route.methods,
                        name,
                        location: route.location,
                    });

                    self.endpoints.push(Endpoint {
                        controller: route.controller,
                        chain,
                    });
                }
                Declaration::Group(mut group, router, location) => {
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
                    group.middleware.extend(router.middleware);
                    let chain = self.inherit(inherited, group.middleware);
                    self.compile(router.declarations, &group_prefix, &name_prefix, &chain, false)?;
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
                    self.fallback = Some((controller, location));
                }
            }
        }
        Ok(())
    }

    fn inherit(&mut self, inherited: &[usize], middleware: Vec<Box<dyn ErasedMiddleware>>) -> Vec<usize> {
        let mut chain = Vec::with_capacity(inherited.len() + middleware.len());
        chain.extend_from_slice(inherited);
        chain.extend(self.middleware.len()..self.middleware.len() + middleware.len());
        self.middleware.extend(middleware);

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
