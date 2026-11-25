//! Scheduling belongs to this host: one bounded desktop worker; browser work
//! runs as queued event-loop jobs without a threads/COOP/COEP requirement.
use crate::{Event, book::Book};
use flow_publication::{
    document::model::flow::Destination,
    publication::{ImportedDocument, Publication},
    reading_session::{
        DocumentRequest, PreparedResource, RequestId, ResolvedLink, ResourceRequest,
    },
};
use winit::event_loop::EventLoopProxy;

pub enum Job {
    #[cfg(not(target_arch = "wasm32"))]
    OpenUrl { generation: u64, url: String },
    #[cfg(target_arch = "wasm32")]
    OpenResponse {
        generation: u64,
        url: String,
        content_type: String,
        bytes: Vec<u8>,
    },
    OpenBytes {
        generation: u64,
        name: String,
        bytes: Vec<u8>,
    },
    #[cfg(not(target_arch = "wasm32"))]
    OpenPath {
        generation: u64,
        path: Option<std::path::PathBuf>,
    },
    Document {
        generation: u64,
        request: DocumentRequest,
    },
    Resource {
        generation: u64,
        request: ResourceRequest,
    },
    Link {
        generation: u64,
        id: RequestId,
        destination: Destination,
    },
}

pub enum Response {
    Opened {
        generation: u64,
        result: Result<Option<(String, Publication)>, String>,
    },
    Document {
        generation: u64,
        id: RequestId,
        result: Result<ImportedDocument, String>,
    },
    Resource {
        generation: u64,
        id: RequestId,
        result: Result<PreparedResource, String>,
    },
    Link {
        generation: u64,
        id: RequestId,
        result: Result<ResolvedLink, String>,
    },
}

pub struct QueuedJob {
    job: Job,
    #[cfg(feature = "telemetry")]
    enqueued_at: web_time::Instant,
}

impl QueuedJob {
    fn new(job: Job) -> Self {
        Self {
            job,
            #[cfg(feature = "telemetry")]
            enqueued_at: web_time::Instant::now(),
        }
    }
}

#[cfg(feature = "telemetry")]
impl Job {
    fn name(&self) -> &'static str {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::OpenUrl { .. } => "open_url",
            #[cfg(target_arch = "wasm32")]
            Self::OpenResponse { .. } => "open_response",
            Self::OpenBytes { .. } => "open_bytes",
            #[cfg(not(target_arch = "wasm32"))]
            Self::OpenPath { .. } => "open_path",
            Self::Document { .. } => "document",
            Self::Resource { .. } => "resource",
            Self::Link { .. } => "link",
        }
    }
    fn generation(&self) -> u64 {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::OpenUrl { generation, .. } | Self::OpenPath { generation, .. } => *generation,
            #[cfg(target_arch = "wasm32")]
            Self::OpenResponse { generation, .. } => *generation,
            Self::OpenBytes { generation, .. }
            | Self::Document { generation, .. }
            | Self::Resource { generation, .. }
            | Self::Link { generation, .. } => *generation,
        }
    }
}

fn process_queued(book: &mut Option<(u64, Book)>, queued: QueuedJob) -> Response {
    let _span = crate::telemetry::phase_span!(
        "flow.reader.job",
        operation = queued.job.name(),
        generation = queued.job.generation(),
        queue_ms = queued.enqueued_at.elapsed().as_secs_f64() * 1000.0
    );
    process(book, queued.job)
}

pub struct Backend {
    #[cfg(not(target_arch = "wasm32"))]
    sender: std::sync::mpsc::SyncSender<QueuedJob>,
    #[cfg(target_arch = "wasm32")]
    proxy: EventLoopProxy<Event>,
    #[cfg(target_arch = "wasm32")]
    book: Option<(u64, Book)>,
}

impl Backend {
    pub fn new(proxy: EventLoopProxy<Event>) -> anyhow::Result<Self> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (sender, receiver) = std::sync::mpsc::sync_channel(64);
            std::thread::Builder::new()
                .name("flow-demo-input".into())
                .spawn(move || {
                    let mut book = None;
                    while let Ok(job) = receiver.recv() {
                        if proxy
                            .send_event(Event::Response(process_queued(&mut book, job)))
                            .is_err()
                        {
                            break;
                        }
                    }
                })?;
            Ok(Self { sender })
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(Self { proxy, book: None })
        }
    }

    pub fn submit(&self, job: Job) -> anyhow::Result<()> {
        let job = QueuedJob::new(job);
        #[cfg(not(target_arch = "wasm32"))]
        self.sender
            .try_send(job)
            .map_err(|_| anyhow::anyhow!("Demo input worker is busy or stopped"))?;
        #[cfg(target_arch = "wasm32")]
        self.proxy
            .send_event(Event::Run(job))
            .map_err(|_| anyhow::anyhow!("Reader event loop stopped"))?;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub fn run(&mut self, job: QueuedJob) -> Response {
        process_queued(&mut self.book, job)
    }
}

fn process(book: &mut Option<(u64, Book)>, job: Job) -> Response {
    match job {
        #[cfg(not(target_arch = "wasm32"))]
        Job::OpenUrl { generation, url } => {
            opened(book, generation, Book::from_url(&url, generation).map(Some))
        }
        #[cfg(target_arch = "wasm32")]
        Job::OpenResponse {
            generation,
            url,
            content_type,
            bytes,
        } => opened(
            book,
            generation,
            Book::from_response(&url, &content_type, bytes, generation).map(Some),
        ),
        Job::OpenBytes {
            generation,
            name,
            bytes,
        } => opened(
            book,
            generation,
            Book::from_bytes(&name, bytes, generation).map(Some),
        ),
        #[cfg(not(target_arch = "wasm32"))]
        Job::OpenPath { generation, path } => {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter(
                        "Documents",
                        &["md", "markdown", "xhtml", "html", "htm", "epub"],
                    )
                    .pick_file()
            });
            let result = path
                .map(|path| Book::from_path(&path, generation))
                .transpose();
            opened(book, generation, result)
        }
        Job::Document {
            generation,
            request,
        } => Response::Document {
            generation,
            id: request.id,
            result: current(book, generation)
                .and_then(|book| book.document(&request))
                .map_err(|error| format!("{error:#}")),
        },
        Job::Resource {
            generation,
            request,
        } => Response::Resource {
            generation,
            id: request.id,
            result: current(book, generation)
                .and_then(|book| book.resource(&request))
                .map_err(|error| format!("{error:#}")),
        },
        Job::Link {
            generation,
            id,
            destination,
        } => Response::Link {
            generation,
            id,
            result: current(book, generation)
                .and_then(|book| book.resolve_link(destination))
                .map_err(|error| format!("{error:#}")),
        },
    }
}

fn current(book: &Option<(u64, Book)>, generation: u64) -> anyhow::Result<&Book> {
    book.as_ref()
        .filter(|(epoch, _)| *epoch == generation)
        .map(|(_, book)| book)
        .ok_or_else(|| anyhow::anyhow!("Obsolete open document"))
}
fn opened(
    book: &mut Option<(u64, Book)>,
    generation: u64,
    result: anyhow::Result<Option<Book>>,
) -> Response {
    let result = result
        .map(|opened| {
            opened.map(|opened| {
                let title = opened.title.clone();
                let model = opened.model.clone();
                *book = Some((generation, opened));
                (title, model)
            })
        })
        .map_err(|error| format!("{error:#}"));
    Response::Opened { generation, result }
}
