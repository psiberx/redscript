use std::fmt;

use peg::error::ExpectedSet;
use redscript::ast::{Seq, Span};
use redscript::bundle::PoolIndex;
use redscript::definition::{Function, FunctionFlags};
use thiserror::Error;

use crate::error::{Cause, Error};
use crate::source_map::Files;
use crate::typechecker::{TypedAst, TypedExpr};

pub mod invalid_temp_use;
pub mod missing_return;
pub mod stmt_fallthrough;
pub mod unused_local;

#[derive(Debug, Error)]
pub enum Diagnostic {
    #[error(
        "this method replacement overwrites a previous annotation targeting the same method, \
         only one replacement per method can be active at a time"
    )]
    ReplaceMethodConflict(PoolIndex<Function>, Span),
    #[error("a field with this name is already defined in the class, this will have no effect")]
    FieldConflict(Span),
    #[error("{0}")]
    Deprecation(Deprecation, Span),
    #[error("this variable is never used")]
    UnusedLocal(Span),
    #[error("not all code paths return a value, make sure you're not missing a return statement")]
    MissingReturn(Span),
    #[error(
        "the body of this case might fall through, it should end with a break/return statement \
         or contain no statements at all"
    )]
    StatementFallthrough(Span),
    #[error(
        "this use of a temporary value is not allowed, consider extracting the highlighted \
         expression into a variable"
    )]
    InvalidUseOfTemporary(Span),
    #[error(
        "this annotation adds a method that conflicts with an existing method in the class, \
         it might cause a runtime error"
    )]
    AddMethodConflict(Span),
    #[error(
        "the type here contains a reference to a non-class type, refs and wrefs must always point \
         to a class, future versions of the compiler will reject this code"
    )]
    NonClassRefDeprecation(Span),
    #[error(
        "the type here contains a class with no indirection, class types must be used through ref \
         or wref, future versions of the compiler will reject this code"
    )]
    ClassWithNoIndirectionDeprecation(Span),
    #[error("syntax error, expected {0}")]
    SyntaxError(ExpectedSet, Span),
    #[error("{0}")]
    CompileError(Cause, Span),
    #[error("compile-time expression error: {0}")]
    CteError(&'static str, Span),
}

impl Diagnostic {
    pub fn log(&self, files: &Files) {
        if self.is_fatal() {
            log::error!("{}", self.display(files));
        } else {
            log::warn!("{}", self.display(files));
        }
    }

    pub fn display<'a>(&'a self, files: &'a Files) -> impl fmt::Display + 'a {
        DisplayFn::new(move |f: &mut fmt::Formatter<'_>| {
            let loc = files.lookup(self.span()).expect("Unknown file");
            let line = loc.enclosing_line().trim_end().replace('\t', " ");
            let underline_len = if loc.start.line == loc.end.line {
                (loc.end.col - loc.start.col).max(1)
            } else {
                3
            };

            if let Self::CompileError(cause, _) = self {
                write!(f, "[{}] ", cause.code())?;
            }

            writeln!(f, "At {loc}:",)?;
            writeln!(f, "{line}")?;
            writeln!(f, "{:w$}{:^<underline_len$}", "", "", w = loc.start.col)?;

            if let Self::CompileError(cause, _) = self {
                writeln!(f, "{}", cause.display(files))
            } else {
                writeln!(f, "{self}")
            }
        })
    }

    pub fn from_error(error: Error) -> Result<Self, Error> {
        match error {
            Error::SyntaxError(set, pos) => Ok(Self::SyntaxError(set, pos)),
            Error::CompileError(cause, pos) => Ok(Self::CompileError(cause, pos)),
            Error::CteError(msg, pos) => Ok(Self::CteError(msg, pos)),
            other => Err(other),
        }
    }

    #[inline]
    pub fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Self::ReplaceMethodConflict(_, _)
                | Self::FieldConflict(_)
                | Self::Deprecation(_, _)
                | Self::UnusedLocal(_)
                | Self::MissingReturn(_)
                | Self::AddMethodConflict(_)
                | Self::NonClassRefDeprecation(_)
                | Self::ClassWithNoIndirectionDeprecation(_)
        )
    }

    #[inline]
    pub fn span(&self) -> Span {
        match self {
            Self::ReplaceMethodConflict(_, span)
            | Self::FieldConflict(span)
            | Self::Deprecation(_, span)
            | Self::UnusedLocal(span)
            | Self::MissingReturn(span)
            | Self::StatementFallthrough(span)
            | Self::InvalidUseOfTemporary(span)
            | Self::AddMethodConflict(span)
            | Self::NonClassRefDeprecation(span)
            | Self::ClassWithNoIndirectionDeprecation(span)
            | Self::CompileError(_, span)
            | Self::SyntaxError(_, span)
            | Self::CteError(_, span) => *span,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::CompileError(cause, _) => cause.code(),
            _ => "OTHER",
        }
    }
}

#[derive(Debug)]
pub enum Deprecation {
    UnrelatedTypeEquals,
}

impl fmt::Display for Deprecation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnrelatedTypeEquals => {
                f.write_str("comparing unrelated types, this is will not be allowed in the future")
            }
        }
    }
}

pub trait DiagnosticPass: fmt::Debug {
    fn diagnose(&self, body: &Seq<TypedAst>, metadata: &FunctionMetadata) -> Vec<Diagnostic>;
}

pub trait ExprDiagnosticPass: fmt::Debug {
    fn diagnose(&self, body: &TypedExpr, metadata: &FunctionMetadata, results: &mut Vec<Diagnostic>);
}

impl<A: ExprDiagnosticPass> DiagnosticPass for A {
    fn diagnose(&self, body: &Seq<TypedAst>, metadata: &FunctionMetadata) -> Vec<Diagnostic> {
        let mut results = vec![];
        for expr in &body.exprs {
            self.diagnose(expr, metadata, &mut results);
        }
        results
    }
}

pub struct FunctionMetadata {
    flags: FunctionFlags,
    was_callback: bool,
    span: Span,
}

impl FunctionMetadata {
    pub fn new(flags: FunctionFlags, was_callback: bool, span: Span) -> Self {
        Self {
            flags,
            was_callback,
            span,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DisplayFn<F>(F);

impl<F> DisplayFn<F> {
    pub fn new(f: F) -> Self {
        Self(f)
    }
}

impl<F> fmt::Display for DisplayFn<F>
where
    F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0(f)
    }
}
