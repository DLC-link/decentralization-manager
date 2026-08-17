use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct Coupon {
    pub id: String,
    pub user_id: String,
    pub amount: f64,
    pub is_active: bool,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub status: CouponStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CouponStatus {
    Active,
    Assigned,
    Expired,
    Claimed,
}

#[derive(Debug, Clone)]
pub struct RewardConfig {
    pub batch_size: usize,
    pub check_interval: Duration,
    pub health_check_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct HealthSignal {
    pub service_name: &'static str,
    pub last_check: Instant,
    pub is_healthy: bool,
}

pub struct RewardAutomation {
    config: RewardConfig,
    coupons: RwLock<Vec<Coupon>>,
    health_signals: Mutex<HealthSignal>,
    tasks: Mutex<HashMap<String, JoinHandle<Result<(), anyhow::Error>>>>,
    notify_channel: mpsc::Sender<Outcome>,
}

pub enum Outcome {
    CouponAssigned { coupon: Coupon },
    CouponExpiringSoon { coupon: Coupon },
    CouponExpired { coupon: Coupon },
    BatchProcessed { count: usize },
}

impl RewardAutomation {
    pub fn new(config: RewardConfig) -> Self {
        RewardAutomation {
            config,
            coupons: RwLock::new(Vec::new()),
            health_signals: Mutex::new(HealthSignal {
                service_name: "reward_automation",
                last_check: Instant::now(),
                is_healthy: true,
            }),
            tasks: Mutex::new(HashMap::new()),
            notify_channel: mpsc::channel(32).0,
        }
    }

    pub async fn spawn_health_monitor(
        mut self,
    ) -> Result<JoinHandle<Result<(), anyhow::Error>>> {
        let health_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            let mut last_batch_check: Option<Instant> = None;

            let run_health_loop = || async {
                let mut batch_start: Option<Instant> = None;

                let mut check_batch_interval = tokio::time::interval(Duration::from_secs(30));

                while !self.tasks.lock().await.is_empty() || batch_start.is_none() {
                    let now = Instant::now();
                    if last_batch_check.is_none() {
                        last_batch_check = Some(now);
                    }

                    // Check for batch processing
                    if let Some(start) = batch_start {
                        if now - start > Duration::from_secs(self.config.check_interval.as_secs()) {
                            let count = self.coupons.read().await.len();
                            if count > 0 {
                                self.notify_channel.send(Outcome::BatchProcessed { count }).await.ok();
                            }
                            batch_start = Some(now);
                        }
                    }

                    // Monitor coupons
                    let coupons = self.coupons.read().await;
                    for coupon in coupons.iter() {
                        if coupon.is_active {
                            self.notify_channel.send(Outcome::CouponAssigned { coupon: coupon.clone() })
                                .await
                                .ok();
                        } else if let CouponStatus::Active = coupon.status {
                            if let Some(expires) = coupon.expires_at {
                                if now - expires > Duration::from_secs(3600) {
                                    self.notify_channel.send(Outcome::CouponExpiringSoon { coupon: coupon.clone() })
                                        .await
                                        .ok();
                                }
                            }
                        }
                    }

                    interval.tick().await;
                }
                Ok(())
            };

            let handle = tokio::spawn(run_health_loop);
            self.tasks.insert("health_monitor".to_string(), handle);
            self.tasks.lock().await.get("health_monitor").unwrap().clone()
        });

        self.tasks.lock().await.insert("health_monitor".to_string(), health_handle);
        self.tasks.lock().await.get("health_monitor").unwrap().clone()
    }

    pub async fn spawn_assigner(mut self) -> Result<JoinHandle<Result<(), anyhow::Error>>> {
        let assign_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));

            while {
                // Logic to check and assign coupons
                let start = Instant::now();

                // Simulate assigning or processing logic
                if let Some(coupon) = self.coupons.read().await.first() {
                    if coupon.is_active && (coupon.amount > 0.0) {
                        let now = Instant::now();
                        if now - coupon.created_at > Duration::from_secs(self.config.check_interval.as_secs()) {
                            // Re-check expiry
                            if let Some(expires) = coupon.expires_at {
                                let is_stale = now - expires > Duration::from_secs(60);
                                if is_stale {
                                    // Coupon ready for re-assignment
                                }
                            }
                        }
                    }
                }

                // Tick interval
                interval.tick().await;
                !self.tasks.lock().await.is_empty()
            }
        });

        self.tasks.lock().await.insert("assigner".to_string(), assign_handle);
        self.tasks.lock().await.get("assigner").unwrap().clone()
    }

    pub async fn spawn_expiration_watcher(mut self) -> Result<JoinHandle<Result<(), anyhow::Error>>> {
        let watcher_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));

            while {
                let now = Instant::now();
                let coupons = self.coupons.read().await;

                let mut updated = Vec::new();

                for coupon in coupons.iter() {
                    if let Some(expires) = coupon.expires_at {
                        if now - expires > Duration::from_secs(60) {
                            if let Some(_expires) = coupon.expires_at {
                                // Handle stale coupon logic
                            }
                        }
                    }
                }

                interval.tick().await;
                !self.tasks.lock().await.is_empty()
            }
        });

        self.tasks.lock().await.insert("expiration_watcher".to_string(), watcher_handle);
        self.tasks.lock().await.get("expiration_watcher").unwrap().clone()
    }

    pub async fn spawn_batch_processor(mut self) -> Result<JoinHandle<Result<(), anyhow::Error>>> {
        let batch_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(200));

            while {
                if self.config.batch_size > 1 {
                    let now = Instant::now();
                    let count = self.coupons.read().await.len();

                    // Check if batch processing is needed
                    if count > 0 {
                        if now - batch_start.unwrap_or(now) > Duration::from_millis(500) {
                            let _ = self.coupons.write().await;
                            batch_start = Some(now);
                        }
                    }
                }

                interval.tick().await;
                !self.tasks.lock().await.is_empty()
            }
        });

        self.tasks.lock().await.insert("batch_processor".to_string(), batch_handle);
        self.tasks.lock().await.get("batch_processor").unwrap().clone()
    }

    pub async fn update_coupon(&self, coupon: Coupon) {
        let mut coupons = self.coupons.write().await;
        let index = coupons.iter().position(|c| c.id == coupon.id);

        if let Some(idx) = index {
            coupons[idx] = coupon;
        } else {
            coupons.push(coupon);
        }
    }

    pub async fn spawn_health_monitor(&self) -> Result<JoinHandle<Result<(), anyhow::Error>>> {
        let health_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            while !self.tasks.lock().await.is_empty() {
                let now = Instant::now();
                self.health_signals
                    .lock()
                    .await
                    .last_check = now;

                interval.tick().await;
            }

            Ok(())
        });

        self.tasks.lock().await.insert("health_monitor".to_string(), health_handle);
        self.tasks.lock().await.get("health_monitor").unwrap().clone()
    }

    pub async fn check_health(&self) -> HealthSignal {
        let signal = self.health_signals.lock().await.clone();
        signal
    }

    pub async fn subscribe_to_outcomes(&self) -> Result<mpsc::Receiver<Outcome>> {
        let (tx, rx) = mpsc::channel(64);
        let _ = self.notify_channel.send(Outcome::BatchProcessed { count: 0 }).await;

        Ok(rx)
    }

    pub async fn run(&self) -> Result<()> {
        let health = self.spawn_health_monitor().await?;

        if !self.config.health_check_enabled {
            return Ok(());
        }

        while let Ok(signal) = self.check_health().await {
            if !signal.is_healthy {
                eprintln!("Reward automation health check failed: {:?}", signal);
            }
        }

        Ok(())
    }
}

pub fn create_reward_automation(config: RewardConfig) -> RewardAutomation {
    RewardAutomation::new(config)
}