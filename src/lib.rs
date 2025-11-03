pub mod config;
pub mod error;
pub mod steps;
pub mod utils;

pub mod proto {
    pub mod com {
        pub mod digitalasset {
            pub mod canton {
                pub mod admin {
                    pub mod crypto {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.admin.crypto.v30");
                        }
                    }
                    pub mod health {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.admin.health.v30");
                        }
                    }
                    pub mod participant {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.admin.participant.v30");
                        }
                    }
                    pub mod pruning {
                        pub mod v30 {
                            tonic::include_proto!("com.digitalasset.canton.admin.pruning.v30");
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
