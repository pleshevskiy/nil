use crate::FileRange;
use core::fmt;
use syntax::{ErrorKind as SynErrorKind, TextRange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: TextRange,
    pub kind: DiagnosticKind,
    pub notes: Vec<(FileRange, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    // Syntax.
    SyntaxError(SynErrorKind),

    // Lowering.
    InvalidDynamic,
    DuplicatedKey,
    EmptyInherit,
    EmptyLetIn,
    LetAttrset,
    UriLiteral,
    MergePlainRecAttrset,
    MergeRecAttrset,

    // Name resolution.
    UndefinedName,

    // Liveness.
    UnusedBinding,
    UnusedWith,
    UnusedRec,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    IncompleteSyntax,
}

impl Diagnostic {
    pub fn new(range: TextRange, kind: DiagnosticKind) -> Self {
        Self {
            range,
            kind,
            notes: Vec::new(),
        }
    }

    pub fn with_note(mut self, frange: FileRange, message: impl Into<String>) -> Self {
        self.notes.push((frange, message.into()));
        self
    }

    pub fn severity(&self) -> Severity {
        match self.kind {
            DiagnosticKind::InvalidDynamic
            | DiagnosticKind::DuplicatedKey
            | DiagnosticKind::UndefinedName => Severity::Error,
            DiagnosticKind::EmptyInherit
            | DiagnosticKind::EmptyLetIn
            | DiagnosticKind::LetAttrset
            | DiagnosticKind::UriLiteral
            | DiagnosticKind::MergePlainRecAttrset
            | DiagnosticKind::MergeRecAttrset
            | DiagnosticKind::UnusedBinding
            | DiagnosticKind::UnusedWith
            | DiagnosticKind::UnusedRec => Severity::Warning,
            DiagnosticKind::SyntaxError(kind) => match kind {
                SynErrorKind::MultipleRoots
                | SynErrorKind::PathTrailingSlash
                | SynErrorKind::PathDuplicatedSlashes
                | SynErrorKind::MultipleNoAssoc => Severity::Error,
                SynErrorKind::MissingToken(_)
                | SynErrorKind::MissingExpr
                | SynErrorKind::MissingElemExpr
                | SynErrorKind::MissingAttr
                | SynErrorKind::MissingParamIdent
                | SynErrorKind::MissingBinding
                | SynErrorKind::NestTooDeep => Severity::IncompleteSyntax,
            },
        }
    }

    pub fn message(&self) -> String {
        match self.kind {
            DiagnosticKind::SyntaxError(kind) => return kind.to_string(),

            DiagnosticKind::InvalidDynamic => "Invalid location of dynamic attribute",
            DiagnosticKind::DuplicatedKey => "Duplicated name definition",
            DiagnosticKind::EmptyInherit => "Nothing inherited",
            DiagnosticKind::EmptyLetIn => "Empty let-in",
            DiagnosticKind::LetAttrset => {
                "`let { ... }` is deprecated. Use `let ... in ...` instead"
            }
            DiagnosticKind::UriLiteral => {
                "URL literal is confusing and deprecated. Use strings instead"
            }
            DiagnosticKind::MergePlainRecAttrset => {
                "Merging non-rec-attrset with rec-attrset, the latter `rec` is implicitly ignored"
            }
            DiagnosticKind::MergeRecAttrset => {
                "Merging rec-attrset with other attrsets or attrpath. Merged values can unexpectedly reference each other remotely as in a single `rec { ... }`"
            }

            DiagnosticKind::UndefinedName => "Undefined name",

            DiagnosticKind::UnusedBinding => "Unused binding",
            DiagnosticKind::UnusedWith => "Unused `with`",
            DiagnosticKind::UnusedRec => "Unused `rec`",
        }
        .into()
    }

    pub fn is_unnecessary(&self) -> bool {
        matches!(
            self.kind,
            DiagnosticKind::EmptyInherit
                | DiagnosticKind::UnusedBinding
                | DiagnosticKind::UnusedWith
                | DiagnosticKind::UnusedRec
        )
    }

    pub fn is_deprecated(&self) -> bool {
        matches!(
            self.kind,
            DiagnosticKind::LetAttrset | DiagnosticKind::UriLiteral
        )
    }

    pub fn debug_display(&self) -> impl fmt::Display + '_ {
        struct Wrapper<'a>(&'a Diagnostic);
        impl fmt::Display for Wrapper<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{:?}: {}", self.0.range, self.0.message(),)?;
                for (frange, msg) in &self.0.notes {
                    // Currently all related information is in the same file.
                    // Ignore the FileId here.
                    write!(f, "\n  {:?}: {}", frange.range, msg)?;
                }
                Ok(())
            }
        }
        Wrapper(self)
    }
}

impl From<syntax::Error> for Diagnostic {
    fn from(err: syntax::Error) -> Self {
        Self::new(err.range, DiagnosticKind::SyntaxError(err.kind))
    }
}
