/// Readiness middleware for services to make them only available after some conditions
pub mod ready;

/// Origin middleware for services so users can trace the real origin of response
/// It is made to prevent some possible malicious behaviour from peers,
/// e.g. peer registers foreign service as own one and does some harmful for the network stuff
// TODO: Origin middleware
pub mod origin;
