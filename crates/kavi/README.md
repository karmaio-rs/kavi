# Kavi

> Compose beautiful web apps in async rust

> Status: work in progress. Not yet ready for production use

Kavi is a full-stack web framework for Rust. Its built on top of [karmaio](https://github.com/karmaio-rs/karmaio) (completion-based async runtime) and [vakya](https://github.com/karmaio-rs/vakya) (HTTP library, karmaio-native).

Cookie support is opt-in through the `cookies` feature. Register
`http::middleware::CookieMiddleware` for queued response cookies, or use
`http::ResponseExt::set_cookie` to append cookies explicitly without middleware.
