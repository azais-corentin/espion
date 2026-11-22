//! The page's side of the viewer: the hub every page connects through (one page at a time:
//! a new one replaces the old), and the HTTP server that serves the page and its WebSocket.
//!
//! The server runs axum on its own thread with a current-thread tokio runtime, bound to a
//! local address (the page reaches it through a reverse proxy such as `tailscale serve`).
//! `GET /` is the page (`assets/screen-target/index.html`), `GET /ws` its WebSocket. Pages that are
//! not a WebSocket (the tests' page) connect to the hub directly.

use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::header,
    response::IntoResponse,
    routing::get,
};
use tokio::sync::{mpsc as amc, oneshot};

use super::link::{FromPage, ToPage};

/// The page, embedded.
const PAGE: &str = include_str!("../../assets/screen-target/index.html");

/// What happened on a page's connection, with when it reached the viewer.
#[derive(Debug, Clone)]
pub struct PageEvent {
    /// The connection: increases with each page.
    pub page: u64,
    pub at: Instant,
    pub kind: PageEventKind,
}

#[derive(Debug, Clone)]
pub enum PageEventKind {
    Open,
    Message(FromPage),
    /// A message that is not one of [`FromPage`].
    Garbled(String),
    Closed,
}

/// Something that watches the page's traffic (the fake camera's simulated screen).
pub trait PageTap: Send + Sync {
    /// A message sent to the current page.
    fn sent(&self, m: &ToPage, at: Instant);
    /// A message from the current page.
    fn received(&self, m: &FromPage, at: Instant);
}

/// The page connections. Cheap to clone.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<HubInner>,
}

struct HubInner {
    events: Mutex<mpsc::Sender<PageEvent>>,
    /// The current page and its outbox.
    current: Mutex<Option<(u64, amc::UnboundedSender<ToPage>)>>,
    next: AtomicU64,
    wake: Box<dyn Fn() + Send + Sync>,
    tap: Mutex<Option<Arc<dyn PageTap>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A page's end of the hub: what the viewer sends it, and the way to answer.
pub struct PageEnd {
    pub id: u64,
    pub inbox: amc::UnboundedReceiver<ToPage>,
    hub: Hub,
}

impl PageEnd {
    /// Sends `m` to the viewer, received now.
    pub fn send(&self, m: FromPage) {
        self.hub.received(self.id, m, Instant::now());
    }
}

impl Drop for PageEnd {
    fn drop(&mut self) {
        self.hub.close(self.id);
    }
}

impl Hub {
    /// A hub whose events go to the returned receiver; `wake` is called after each.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> (Self, mpsc::Receiver<PageEvent>) {
        let (tx, rx) = mpsc::channel();
        let hub = Self {
            inner: Arc::new(HubInner {
                events: Mutex::new(tx),
                current: Mutex::new(None),
                next: AtomicU64::new(1),
                wake: Box::new(wake),
                tap: Mutex::new(None),
            }),
        };
        (hub, rx)
    }

    /// Watches the traffic of every page from now on.
    pub fn set_tap(&self, tap: Arc<dyn PageTap>) {
        *lock(&self.inner.tap) = Some(tap);
    }

    fn event(&self, page: u64, at: Instant, kind: PageEventKind) {
        // The viewer may be gone; the page then talks to no one.
        let _ = lock(&self.inner.events).send(PageEvent { page, at, kind });
        (self.inner.wake)();
    }

    /// Connects a new page; it replaces the current one, whose outbox closes.
    pub fn open(&self) -> PageEnd {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = amc::unbounded_channel();
        *lock(&self.inner.current) = Some((id, tx));
        self.event(id, Instant::now(), PageEventKind::Open);
        PageEnd {
            id,
            inbox: rx,
            hub: self.clone(),
        }
    }

    fn received(&self, page: u64, m: FromPage, at: Instant) {
        if self.current() != Some(page) {
            return;
        }
        if let Some(tap) = lock(&self.inner.tap).clone() {
            tap.received(&m, at);
        }
        self.event(page, at, PageEventKind::Message(m));
    }

    fn garbled(&self, page: u64, text: String) {
        if self.current() == Some(page) {
            self.event(page, Instant::now(), PageEventKind::Garbled(text));
        }
    }

    fn close(&self, page: u64) {
        let mut current = lock(&self.inner.current);
        if current.as_ref().is_some_and(|(id, _)| *id == page) {
            *current = None;
            drop(current);
            self.event(page, Instant::now(), PageEventKind::Closed);
        }
    }

    /// The current page.
    pub fn current(&self) -> Option<u64> {
        lock(&self.inner.current).as_ref().map(|(id, _)| *id)
    }

    /// Sends `m` to the current page; false without one.
    pub fn send(&self, m: ToPage) -> bool {
        let current = lock(&self.inner.current);
        let Some((_, tx)) = current.as_ref() else {
            return false;
        };
        if let Some(tap) = lock(&self.inner.tap).clone() {
            tap.sent(&m, Instant::now());
        }
        tx.send(m).is_ok()
    }
}

/// The running HTTP server; stops when dropped.
pub struct Server {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Binds `listen` and serves the page and its WebSocket through `hub`.
    pub fn start(listen: SocketAddr, hub: Hub) -> io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let listener = rt.block_on(tokio::net::TcpListener::bind(listen))?;
        let addr = listener.local_addr()?;
        let (stop, stopped) = oneshot::channel();
        let app = Router::new()
            .route("/", get(page))
            .route("/ws", get(socket))
            .with_state(hub);
        let thread = thread::Builder::new()
            .name("target-http".into())
            .spawn(move || {
                rt.block_on(async move {
                    tokio::select! {
                        r = axum::serve(listener, app) => {
                            if let Err(e) = r {
                                eprintln!("screen target server: {e}");
                            }
                        }
                        _ = stopped => {}
                    }
                });
                // Dropping the runtime ends the pages' connections.
            })?;
        Ok(Self {
            addr,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// Where it listens.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

async fn page() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        PAGE,
    )
}

async fn socket(ws: WebSocketUpgrade, State(hub): State<Hub>) -> impl IntoResponse {
    ws.on_upgrade(move |s| connection(s, hub))
}

/// One page's connection: its messages to the hub, the hub's to it, until either side ends.
async fn connection(mut ws: WebSocket, hub: Hub) {
    let mut end = hub.open();
    loop {
        tokio::select! {
            m = ws.recv() => match m {
                Some(Ok(Message::Text(t))) => match serde_json::from_str::<FromPage>(&t) {
                    Ok(m) => hub.received(end.id, m, Instant::now()),
                    Err(e) => hub.garbled(end.id, format!("{e}: {}", t.as_str())),
                },
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            m = end.inbox.recv() => match m {
                Some(m) => {
                    let text = serde_json::to_string(&m).expect("messages serialise");
                    if ws.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                // Another page replaced this one.
                None => break,
            },
        }
    }
}
