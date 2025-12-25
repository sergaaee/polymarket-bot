use lazy_static::lazy_static;
use prometheus::{
    CounterVec, HistogramVec, IntCounterVec, IntGaugeVec, register_counter_vec,
    register_histogram_vec, register_int_counter_vec, register_int_gauge_vec,
};

lazy_static! {
    // 🔹 HTTP / API latency
    /// latency всех запросов
    pub static ref REQUEST_LATENCY: HistogramVec =
        register_histogram_vec!(
            "pm_request_latency_seconds",
            "Latency of polymarket requests",
            &["service", "method"]
        ).unwrap();


    // 🔹 Orders
    /// сколько ордеров отправили
    pub static ref ORDERS_TOTAL: CounterVec =
        register_counter_vec!(
            "pm_orders_total",
            "Total orders created",
            &["asset"]
        ).unwrap();

    /// сколько смэтчились
    pub static ref ORDERS_MATCHED_TOTAL: CounterVec =
        register_counter_vec!(
            "pm_orders_matched_total",
            "Matched orders",
            &["asset"]
        ).unwrap();

    /// stop-loss срабатывания
    pub static ref STOP_LOSS_TOTAL: CounterVec =
        register_counter_vec!(
            "pm_stop_loss_total",
            "Stop-loss triggered",
            &["asset"]
        ).unwrap();


    pub static ref ORDERS_CANCELLED_TOTAL: CounterVec =
        register_counter_vec!(
            "orders_cancelled_total",
            "Orders cancelled",
            &["asset"]
        ).unwrap();


    pub static ref HEDGE_ORDERS_TOTAL: CounterVec =
        register_counter_vec!(
            "hedge_orders_total",
            "Hedge orders created",
            &["asset"]
        ).unwrap();


    pub static ref HEDGE_ORDERS_CANCELLED_TOTAL: CounterVec =
        register_counter_vec!(
            "hedge_orders_cancelled_total",
            "Hedge orders cancelled",
            &["asset"]
        ).unwrap();

    pub static ref HEDGE_ORDERS_PARTIAL_TOTAL: CounterVec =
        register_counter_vec!(
            "hedge_orders_partial_total",
            "Hedge orders partially matched",
            &["asset"]
    ).unwrap();

    pub static ref HEDGE_ORDERS_MATCHED_TOTAL: CounterVec =
        register_counter_vec!(
            "hedge_orders_matched_total",
            "Hedge orders matched",
            &["asset"]
        ).unwrap();

    pub static ref ORDERS_PARTIAL_TOTAL: CounterVec =
        register_counter_vec!(
            "orders_partial_total",
            "Orders partially matched",
            &["asset"]
        ).unwrap();

    // 🔹 Retry
    pub static ref RETRIES_TOTAL: IntCounterVec =
        register_int_counter_vec!(
            "retries_total",
            "Total retries",
            &["operation"]
        ).unwrap();

    // 🔹 PnL
    pub static ref PNL: IntGaugeVec =
        register_int_gauge_vec!(
            "bot_pnl",
            "Current PnL",
            &["asset"]
        ).unwrap();
}
