use super::{cookies_from_request, set_cookies, Cookie, Key};
use axum::{
    async_trait,
    extract::{FromRequest, RequestParts},
    response::{IntoResponse, IntoResponseParts, Response, ResponseParts},
    Extension,
};
use cookie_lib::PrivateJar;
use std::{convert::Infallible, fmt, marker::PhantomData};

/// Extractor that grabs private cookies from the request and manages the jar.
///
/// All cookies will be private and encrypted with a [`Key`]. This makes it suitable for storing
/// private data.
///
/// Note that methods like [`PrivateCookieJar::add`], [`PrivateCookieJar::remove`], etc updates the
/// [`PrivateCookieJar`] and returns it. This value _must_ be returned from the handler as part of
/// the response for the changes to be propagated.
///
/// # Example
///
/// ```rust
/// use axum::{
///     Router,
///     Extension,
///     routing::{post, get},
///     extract::TypedHeader,
///     response::{IntoResponse, Redirect},
///     headers::authorization::{Authorization, Bearer},
///     http::StatusCode,
/// };
/// use axum_extra::extract::cookie::{PrivateCookieJar, Cookie, Key};
///
/// async fn set_secret(
///     jar: PrivateCookieJar,
/// ) -> (PrivateCookieJar, Redirect) {
///     let updated_jar = jar.add(Cookie::new("secret", "secret-data"));
///     (updated_jar, Redirect::to("/get"))
/// }
///
/// async fn get_secret(jar: PrivateCookieJar) {
///     if let Some(data) = jar.get("secret") {
///         // ...
///     }
/// }
///
/// // Generate a secure key
/// //
/// // You probably don't wanna generate a new one each time the app starts though
/// let key = Key::generate();
///
/// let app = Router::new()
///     .route("/set", post(set_secret))
///     .route("/get", get(get_secret))
///     // add extension with the key so `PrivateCookieJar` can access it
///     .layer(Extension(key));
/// # let app: Router<axum::body::Body> = app;
/// ```
pub struct PrivateCookieJar<K = Key> {
    jar: cookie_lib::CookieJar,
    key: Key,
    // The key used to extract the key extension. Allows users to use multiple keys for different
    // jars. Maybe a library wants its own key.
    _marker: PhantomData<K>,
}

impl<K> fmt::Debug for PrivateCookieJar<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateCookieJar")
            .field("jar", &self.jar)
            .field("key", &"REDACTED")
            .finish()
    }
}

#[async_trait]
impl<B, K> FromRequest<B> for PrivateCookieJar<K>
where
    B: Send,
    K: Into<Key> + Clone + Send + Sync + 'static,
{
    type Rejection = <axum::Extension<K> as FromRequest<B>>::Rejection;

    async fn from_request(req: &mut RequestParts<B>) -> Result<Self, Self::Rejection> {
        let key = Extension::<K>::from_request(req).await?.0.into();

        let mut jar = cookie_lib::CookieJar::new();
        let mut private_jar = jar.private_mut(&key);
        for cookie in cookies_from_request(req) {
            if let Some(cookie) = private_jar.decrypt(cookie) {
                private_jar.add_original(cookie);
            }
        }

        Ok(Self {
            jar,
            key,
            _marker: PhantomData,
        })
    }
}

impl<K> PrivateCookieJar<K> {
    /// Get a cookie from the jar.
    ///
    /// If the cookie exists and can be decrypted then it is returned in plaintext.
    ///
    /// # Example
    ///
    /// ```rust
    /// use axum_extra::extract::cookie::PrivateCookieJar;
    /// use axum::response::IntoResponse;
    ///
    /// async fn handle(jar: PrivateCookieJar) {
    ///     let value: Option<String> = jar
    ///         .get("foo")
    ///         .map(|cookie| cookie.value().to_owned());
    /// }
    /// ```
    pub fn get(&self, name: &str) -> Option<Cookie<'static>> {
        self.private_jar().get(name)
    }

    /// Remove a cookie from the jar.
    ///
    /// # Example
    ///
    /// ```rust
    /// use axum_extra::extract::cookie::{PrivateCookieJar, Cookie};
    /// use axum::response::IntoResponse;
    ///
    /// async fn handle(jar: PrivateCookieJar) -> PrivateCookieJar {
    ///     jar.remove(Cookie::named("foo"))
    /// }
    /// ```
    #[must_use]
    pub fn remove(mut self, cookie: Cookie<'static>) -> Self {
        self.private_jar_mut().remove(cookie);
        self
    }

    /// Add a cookie to the jar.
    ///
    /// The value will automatically be percent-encoded.
    ///
    /// # Example
    ///
    /// ```rust
    /// use axum_extra::extract::cookie::{PrivateCookieJar, Cookie};
    /// use axum::response::IntoResponse;
    ///
    /// async fn handle(jar: PrivateCookieJar) -> PrivateCookieJar {
    ///     jar.add(Cookie::new("foo", "bar"))
    /// }
    /// ```
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, cookie: Cookie<'static>) -> Self {
        self.private_jar_mut().add(cookie);
        self
    }

    /// Authenticates and decrypts `cookie`, returning the plaintext version if decryption succeeds
    /// or `None` otherwise.
    pub fn decrypt(&self, cookie: Cookie<'static>) -> Option<Cookie<'static>> {
        self.private_jar().decrypt(cookie)
    }

    /// Get an iterator over all cookies in the jar.
    ///
    /// Only cookies with valid authenticity and integrity are yielded by the iterator.
    pub fn iter(&self) -> impl Iterator<Item = Cookie<'static>> + '_ {
        PrivateCookieJarIter {
            jar: self,
            iter: self.jar.iter(),
        }
    }

    fn private_jar(&self) -> PrivateJar<&'_ cookie_lib::CookieJar> {
        self.jar.private(&self.key)
    }

    fn private_jar_mut(&mut self) -> PrivateJar<&'_ mut cookie_lib::CookieJar> {
        self.jar.private_mut(&self.key)
    }
}

impl<K> IntoResponseParts for PrivateCookieJar<K> {
    type Error = Infallible;

    fn into_response_parts(self, mut res: ResponseParts) -> Result<ResponseParts, Self::Error> {
        set_cookies(self.jar, res.headers_mut());
        Ok(res)
    }
}

impl<K> IntoResponse for PrivateCookieJar<K> {
    fn into_response(self) -> Response {
        (self, ()).into_response()
    }
}

struct PrivateCookieJarIter<'a, K> {
    jar: &'a PrivateCookieJar<K>,
    iter: cookie_lib::Iter<'a>,
}

impl<'a, K> Iterator for PrivateCookieJarIter<'a, K> {
    type Item = Cookie<'static>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let cookie = self.iter.next()?;

            if let Some(cookie) = self.jar.get(cookie.name()) {
                return Some(cookie);
            }
        }
    }
}
