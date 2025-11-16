pub mod consts;
pub mod dirs;
pub mod error;
pub mod network_config;
pub mod noise;
pub mod steps;
pub mod utils;

pub mod proto {
    pub mod google {
        pub mod rpc {
            tonic::include_proto!("google.rpc");
        }
    }

    pub mod com {
        pub mod daml {
            pub mod ledger {
                pub mod api {
                    pub mod v2 {
                        tonic::include_proto!("com.daml.ledger.api.v2");

                        pub mod admin {
                            tonic::include_proto!("com.daml.ledger.api.v2.admin");
                        }

                        pub mod interactive {
                            tonic::include_proto!("com.daml.ledger.api.v2.interactive");

                            pub mod transaction {
                                pub mod v1 {
                                    tonic::include_proto!(
                                        "com.daml.ledger.api.v2.interactive.transaction.v1"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        pub mod digitalasset {
            pub mod canton {
                pub mod admin {
                    pub mod participant {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.admin.participant.v30");
                        }
                    }
                    pub mod sequencer {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.admin.sequencer.v30");
                        }
                    }
                    pub mod time {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.admin.time.v30");
                        }
                    }
                }
                pub mod crypto {
                    pub mod admin {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.crypto.admin.v30");
                        }
                    }
                    pub mod v30 {
                        tonic::include_proto!("com.digitalasset.canton.crypto.v30");
                    }
                }
                pub mod protocol {
                    pub mod v30 {
                        tonic::include_proto!("com.digitalasset.canton.protocol.v30");
                    }
                }
                pub mod topology {
                    pub mod admin {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.topology.admin.v30");
                        }
                    }
                }
            }
        }
    }
}
