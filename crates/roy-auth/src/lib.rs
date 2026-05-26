//! User/team/invite store + JWT helpers shared by roy-management and roy-gateway.
//! Tables live in the shared `agents.db` next to roy-agents and roy-management
//! (migration versions 10+). The crate exposes a small surface: stores, JWT
//! sign/verify, cookie parsing, and an `Acl` helper.

pub mod acl;
pub mod cookie;
pub mod db;
pub mod invite_store;
pub mod jwt;
pub mod password;
pub mod team_store;
pub mod types;
pub mod user_store;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use acl::{Acl, AclError};
pub use cookie::{verify_cookie, verify_ws_protocol, COOKIE_NAME};
pub use db::apply_migrations;
pub use invite_store::{InviteError, InviteStore};
pub use jwt::{sign_session, verify_session, JwtError};
pub use password::{hash_password, verify_password};
pub use team_store::{TeamStore, TeamStoreError};
pub use types::{
    NewTeam, NewUser, Role, Scope, Team, TeamInvite, TeamMember, TeamMembership, User, UserProfile,
};
pub use user_store::{UserStore, UserStoreError};
