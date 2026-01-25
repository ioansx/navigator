use std::{backtrace::Backtrace, error::Error, fmt::Display};

pub type Resultx<T> = Result<T, Errx>;

#[derive(Debug)]
pub struct Errx {
    pub knd: Kindx,
    pub src: Option<Box<dyn Error>>,
    pub bkt: Option<Backtrace>,
}

impl Errx {
    fn new(src: Option<Box<dyn Error>>, knd: Kindx) -> Self {
        let Some(src) = src else {
            return Self {
                src: None,
                bkt: Some(Backtrace::force_capture()),
                knd,
            };
        };

        match src.downcast::<Errx>() {
            Ok(mut errx) => {
                let bkt = errx.bkt.take();
                Self {
                    src: Some(errx),
                    bkt,
                    knd,
                }
            }
            Err(err) => Self {
                src: Some(err),
                bkt: Some(Backtrace::force_capture()),
                knd,
            },
        }
    }

    pub fn ctx(mut self, knd: Kindx) -> Self {
        let bkt = self.bkt.take();
        Self {
            knd,
            src: Some(Box::new(self)),
            bkt,
        }
    }

    pub fn of(knd: Kindx) -> Self {
        Self::new(None, knd)
    }

    pub fn e_of(src: impl Error + Send + Sync + 'static, knd: Kindx) -> Self {
        Self::new(Some(Box::new(src)), knd)
    }

    pub fn any(msg: impl Into<String>) -> Self {
        Self::new(None, Kindx::Any(msg.into()))
    }

    pub fn e_any(src: impl Error + 'static, msg: impl Into<String>) -> Self {
        Self::new(Some(Box::new(src)), Kindx::Any(msg.into()))
    }

    pub fn e_io(src: impl Error + 'static, msg: impl Into<String>) -> Self {
        Self::new(Some(Box::new(src)), Kindx::Io(msg.into()))
    }

    pub fn chain(&self) -> Vec<String> {
        let mut chain: Vec<String> = Vec::new();
        let mut source = Some(self as &dyn Error);
        while let Some(err) = source {
            chain.push(err.to_string());
            source = err.source();
        }
        chain
    }
}

impl Display for Errx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.knd)
    }
}

impl Error for Errx {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.src.as_deref()
    }
}

#[derive(Clone, Debug)]
pub enum Kindx {
    Any(String),
    Io(String),
}

impl Kindx {
    pub fn any(msg: impl Into<String>) -> Kindx {
        Kindx::Any(msg.into())
    }
}

impl Display for Kindx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kindx::Any(msg) => {
                write!(f, "Any: {}", msg)
            }
            Kindx::Io(msg) => {
                write!(f, "IO: {}", msg)
            }
        }
    }
}

impl From<std::io::Error> for Errx {
    fn from(e: std::io::Error) -> Self {
        Errx::e_of(e, Kindx::Io("IO operation failed".into()))
    }
}
