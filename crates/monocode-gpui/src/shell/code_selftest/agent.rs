//! Steps 11 and 13, and the held session: stub.
use super::*;

impl Driver {
    pub(super) async fn agent(&mut self, _cx: &mut Cx) {}
    pub(super) async fn idle(&mut self, _cx: &mut Cx) {}
    pub(super) async fn hold(&mut self, _cx: &mut Cx) {}
}
