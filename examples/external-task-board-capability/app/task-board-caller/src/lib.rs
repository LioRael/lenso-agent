use external_task_board_audit as audit;
use external_task_board_report as report;
use lenso::Port;

#[lenso::plugin(consumer)]
#[derive(Clone, Debug)]
struct TaskBoardCaller {
    _report: Port<report::TaskBoardReportClient>,
    _audit: Port<audit::TaskBoardAuditClient>,
}

pub fn link() {
    link_plugin();
}
