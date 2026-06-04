use std::future::Future;
use tokio_util::sync::CancellationToken;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CancelErr {
    Cancelled,
}

pub(crate) async fn or_cancel<F>(
    future: F,
    token: &CancellationToken,
) -> Result<F::Output, CancelErr>
where
    F: Future,
{
    tokio::select! {
        _ = token.cancelled() => Err(CancelErr::Cancelled),
        res = future => Ok(res),
    }
}
