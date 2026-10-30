// SPDX-License-Identifier: AGPL-3.0-or-later

//! The process-wide budget for proxied response bytes held in memory.
//!
//! Tauri's responder takes a complete body, so every proxied response is buffered before the
//! renderer sees any of it. Each buffered byte, raw and decoded, holds one permit from the moment
//! it is read until the scheme handler has handed the body to the webview: the permits ride in
//! the response's extensions as a [`HeldBuffer`], which lib.rs's scheme closure takes out and
//! drops right after `responder.respond`. A body that could never fit fails at once.
//!
//! No request waits while it holds permits, so bodies cannot stall each other. A reservation
//! that holds nothing yet waits for what it asks, bounded by the request deadline and
//! cancellation: a body of known length asks for all of it before the first byte is read. A
//! reservation that already holds permits only takes what is free, and a spent budget fails the
//! request then, which is how bodies of unknown length and decoded output grow.

use std::fmt;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub const RESPONSE_BUFFER_BUDGET: usize = 256 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum BudgetError {
    /// The response would need more than the whole budget.
    NeverFits,
    /// The budget was spent while the request held part of it.
    Exhausted,
    Timeout,
    Aborted,
}

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NeverFits => "upstream body exceeds the response buffer budget",
            Self::Exhausted => "response buffer budget ran out while the body was read",
            Self::Timeout => "request timed out waiting for response buffer budget",
            Self::Aborted => "request was aborted waiting for response buffer budget",
        })
    }
}

#[derive(Clone)]
pub struct BufferBudget {
    semaphore: Arc<Semaphore>,
    total: usize,
}

impl BufferBudget {
    pub fn new(total: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(total)),
            total,
        }
    }

    #[cfg(test)]
    pub fn total(&self) -> usize {
        self.total
    }

    #[cfg(test)]
    pub fn available(&self) -> usize {
        self.semaphore.available_permits()
    }
}

/// When a wait for budget gives up: the request deadline, or cancellation.
pub struct BudgetWait {
    pub deadline: Instant,
    pub cancel: CancellationToken,
}

/// The permits of the bytes one response holds, returned to the budget on drop.
#[derive(Default)]
pub struct Reservation(Option<OwnedSemaphorePermit>);

impl Reservation {
    pub fn bytes(&self) -> usize {
        self.0.as_ref().map_or(0, OwnedSemaphorePermit::num_permits)
    }

    /// Adds `bytes` permits. An empty reservation waits for them; one that holds permits takes
    /// them only if they are free.
    pub async fn grow(
        &mut self,
        budget: &BufferBudget,
        bytes: usize,
        wait: &BudgetWait,
    ) -> Result<(), BudgetError> {
        if self.0.is_some() {
            return self.try_grow(budget, bytes);
        }
        let Some(count) = permit_count(self, budget, bytes)? else {
            return Ok(());
        };
        let permit = tokio::select! {
            biased;
            () = wait.cancel.cancelled() => return Err(BudgetError::Aborted),
            () = tokio::time::sleep_until(wait.deadline) => return Err(BudgetError::Timeout),
            permit = budget.semaphore.clone().acquire_many_owned(count) => {
                permit.expect("the buffer budget semaphore is never closed")
            }
        };
        self.0 = Some(permit);
        Ok(())
    }

    /// Adds `bytes` permits if they are free, without waiting.
    pub fn try_grow(&mut self, budget: &BufferBudget, bytes: usize) -> Result<(), BudgetError> {
        let Some(count) = permit_count(self, budget, bytes)? else {
            return Ok(());
        };
        let permit = budget
            .semaphore
            .clone()
            .try_acquire_many_owned(count)
            .map_err(|_| BudgetError::Exhausted)?;
        match &mut self.0 {
            Some(held) => held.merge(permit),
            None => self.0 = Some(permit),
        }
        Ok(())
    }

    /// Returns `bytes` of the held permits to the budget.
    pub fn release(&mut self, bytes: usize) {
        if let Some(held) = &mut self.0 {
            drop(held.split(bytes));
        }
    }
}

/// `None` for nothing to add; `NeverFits` past the whole budget.
fn permit_count(
    held: &Reservation,
    budget: &BufferBudget,
    bytes: usize,
) -> Result<Option<u32>, BudgetError> {
    if bytes == 0 {
        return Ok(None);
    }
    if held.bytes() + bytes > budget.total {
        return Err(BudgetError::NeverFits);
    }
    u32::try_from(bytes)
        .map(Some)
        .map_err(|_| BudgetError::NeverFits)
}

/// A response's [`Reservation`] as a response extension, which has to be `Clone`.
#[derive(Clone)]
pub struct HeldBuffer(
    #[allow(dead_code, reason = "held only to release the permits when dropped")] Arc<Reservation>,
);

impl HeldBuffer {
    pub fn new(reservation: Reservation) -> Self {
        Self(Arc::new(reservation))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn wait_for(duration: Duration) -> BudgetWait {
        BudgetWait {
            deadline: Instant::now() + duration,
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn reservations_hold_permits_until_dropped_or_released() {
        let budget = BufferBudget::new(100);
        let wait = wait_for(Duration::from_secs(5));
        let mut held = Reservation::default();
        held.grow(&budget, 30, &wait).await.unwrap();
        held.grow(&budget, 40, &wait).await.unwrap();
        assert_eq!(held.bytes(), 70);
        assert_eq!(budget.available(), 30);
        held.release(30);
        assert_eq!(held.bytes(), 40);
        assert_eq!(budget.available(), 60);
        drop(held);
        assert_eq!(budget.available(), 100);
    }

    #[tokio::test]
    async fn more_than_the_whole_budget_never_fits() {
        let budget = BufferBudget::new(100);
        let wait = wait_for(Duration::from_secs(5));
        let mut held = Reservation::default();
        held.grow(&budget, 60, &wait).await.unwrap();
        assert_eq!(
            held.grow(&budget, 41, &wait).await,
            Err(BudgetError::NeverFits)
        );
        assert_eq!(held.bytes(), 60);
        assert_eq!(
            Reservation::default().grow(&budget, 101, &wait).await,
            Err(BudgetError::NeverFits)
        );
    }

    #[tokio::test]
    async fn a_reservation_holding_permits_never_waits() {
        let budget = BufferBudget::new(100);
        let wait = wait_for(Duration::from_secs(5));
        let mut first = Reservation::default();
        first.grow(&budget, 50, &wait).await.unwrap();
        let mut second = Reservation::default();
        second.grow(&budget, 40, &wait).await.unwrap();
        let started = Instant::now();
        assert_eq!(
            second.grow(&budget, 20, &wait).await,
            Err(BudgetError::Exhausted)
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(second.bytes(), 40);
        assert_eq!(second.try_grow(&budget, 10), Ok(()));
        assert_eq!(
            Reservation::default().try_grow(&budget, 1),
            Err(BudgetError::Exhausted)
        );
        drop((first, second));
        assert_eq!(budget.available(), 100);
    }

    #[tokio::test]
    async fn a_spent_budget_waits_until_the_deadline() {
        let budget = BufferBudget::new(100);
        let mut first = Reservation::default();
        first
            .grow(&budget, 80, &wait_for(Duration::from_secs(5)))
            .await
            .unwrap();
        let mut second = Reservation::default();
        assert_eq!(
            second
                .grow(&budget, 30, &wait_for(Duration::from_millis(100)))
                .await,
            Err(BudgetError::Timeout)
        );
        assert_eq!(second.bytes(), 0);
        assert_eq!(budget.available(), 20);
    }

    #[tokio::test]
    async fn a_waiting_reservation_resumes_when_permits_return() {
        let budget = BufferBudget::new(100);
        let mut first = Reservation::default();
        first
            .grow(&budget, 80, &wait_for(Duration::from_secs(5)))
            .await
            .unwrap();
        let waiting = {
            let budget = budget.clone();
            tokio::spawn(async move {
                let mut second = Reservation::default();
                second
                    .grow(&budget, 30, &wait_for(Duration::from_secs(5)))
                    .await
                    .map(|()| second)
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        drop(first);
        let second = waiting.await.unwrap().unwrap();
        assert_eq!(second.bytes(), 30);
        assert_eq!(budget.available(), 70);
    }

    #[tokio::test]
    async fn cancellation_ends_the_wait() {
        let budget = BufferBudget::new(100);
        let mut first = Reservation::default();
        first
            .grow(&budget, 100, &wait_for(Duration::from_secs(5)))
            .await
            .unwrap();
        let wait = wait_for(Duration::from_secs(5));
        wait.cancel.cancel();
        let mut second = Reservation::default();
        assert_eq!(
            second.grow(&budget, 1, &wait).await,
            Err(BudgetError::Aborted)
        );
        drop(first);
        assert_eq!(budget.available(), 100);
    }
}
