#[derive(Clone, Debug)]
pub enum SubmitSummaryWithResponses<R> {
    Responses(Vec<R>),
    Errors(usize),
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum AnySubmitSummary<R> {
    Done,
    Responses(Vec<R>),
    Errors(usize),
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
pub enum SubmitSummary {
    Done,
    Errors(usize),
    Shutdown,
}

impl<R> From<AnySubmitSummary<R>> for SubmitSummary {
    fn from(a: AnySubmitSummary<R>) -> Self {
        match a {
            AnySubmitSummary::Done => SubmitSummary::Done,
            AnySubmitSummary::Responses(_) => panic!("cannot convert"),
            AnySubmitSummary::Errors(e) => SubmitSummary::Errors(e),
            AnySubmitSummary::Shutdown => SubmitSummary::Shutdown,
        }
    }
}

impl<R> From<AnySubmitSummary<R>> for SubmitSummaryWithResponses<R> {
    fn from(a: AnySubmitSummary<R>) -> Self {
        match a {
            AnySubmitSummary::Responses(r) => SubmitSummaryWithResponses::Responses(r),
            AnySubmitSummary::Done => panic!("cannot convert"),
            AnySubmitSummary::Errors(e) => SubmitSummaryWithResponses::Errors(e),
            AnySubmitSummary::Shutdown => SubmitSummaryWithResponses::Shutdown,
        }
    }
}

