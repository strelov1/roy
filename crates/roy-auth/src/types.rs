use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewUser;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTeam;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMembership;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamInvite;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    Owner,
    Member,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Scope {
    Personal,
    Team(String),
}
