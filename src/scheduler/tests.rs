use super::*;
use crate::model::Vmmem;

const H: u64 = 1_800_000_000;

/// Historical cases keep the 4 GB Windows margin their figures were taken with; real cases use the defaults.
fn config() -> Config {
    crate::i18n::set(crate::i18n::Lang::Fr);
    Config { margin_ram_windows_mb: 4096, ..Config::default() }
}

fn measures(ram_windows_avail_gb: f64, wsl_avail_gb: f64, vmmem_gb: f64) -> Measures {
    Measures {
        ram_total_mb: 32 * 1024,
        ram_avail_mb: (ram_windows_avail_gb * 1024.0) as u64,
        commit_mb: 0,
        commit_limit_mb: 0,
        cpu_pct: 0.0,
        windows_cores: 16,
        swap_out_ps: 0.0,
        vmmem: Some(Vmmem { pid: 4242, rss_mb: (vmmem_gb * 1024.0) as u64 }),
        wsl: Some(WslMeasures {
            mem_avail_mb: (wsl_avail_gb * 1024.0) as u64,
            mem_total_mb: 11_960,
            load1: 1.0,
            swap_total_mb: 4096,
            swap_free_mb: 4096,
            pswpin: 0,
            pswpout: 0,
            at: H,
        }),
    }
}

fn fresh(mut m: Measures, at: u64) -> Measures {
    if let Some(w) = &mut m.wsl {
        w.at = at;
    }
    m
}

fn large() -> Measures {
    measures(20.0, 8.0, 4.0)
}

fn ticket(id: u64, side: Side, ram_gb: f64, cpu: u32, duration_min: u64) -> Ticket {
    Ticket {
        id,
        request_id: format!("r{id}"),
        side,
        need: Need { ram_mb: (ram_gb * 1024.0) as u64, cpu, duration_s: duration_min * 60 },
        description: format!("task {id}"),
        session: "test".into(),
        claude_session_id: None,
        claude_pid: None,
        dir: "/".into(),
        signature: String::new(),
        state: TicketState::Waiting,
        created: H,
        granted: None,
        avail_at_grant_mb: None,
        manual: false,
        acked: false,
        lease_overrun: false,
        recalled: None,
        orphan_cause: None,
        reattach_key: String::new(),
        project: None,
        branch: None,
        force: false,
        base_ram_windows_mb: None,
        base_ram_wsl_mb: None,
        base_vmmem_mb: None,
        overtakes: 0,
        link: None,
        identity: None,
        lost: None,
        presence: H,
        listeners: 1,
    }
}

fn active(mut t: Ticket, granted: u64, avail_at_grant_gb: f64) -> Ticket {
    t.state = TicketState::Active;
    t.granted = Some(granted);
    t.presence = granted;
    t.avail_at_grant_mb = Some((avail_at_grant_gb * 1024.0) as u64);
    t.acked = true;
    t
}

fn state(tickets: Vec<Ticket>) -> State {
    crate::i18n::set(crate::i18n::Lang::Fr);
    State {
        epoch: 1,
        counter: tickets.iter().map(|t| t.id).max().unwrap_or(0),
        vmmem_pid: Some(4242),
        manual_freeze: None,
        tickets,
        ..State::default()
    }
}

fn present(mut e: State, at: u64) -> State {
    for t in &mut e.tickets {
        t.presence = at;
    }
    e
}

fn state_of(e: &State, id: u64) -> Option<TicketState> {
    e.ticket(id).map(|t| t.state)
}

#[test]
fn empty_queue_immediate_clearance() {
    let e = state(vec![ticket(1, Side::Wsl, 2.0, 2, 10)]);
    let p = schedule(&e, &large(), &config(), H);
    assert_eq!(p.events, vec![Event::Cleared(1)]);
    assert_eq!(state_of(&p.state, 1), Some(TicketState::Active));
    assert_eq!(p.reasons[&1].decision, DecisionKind::Cleared);
    assert!(p.persist);
}

#[test]
fn several_clearances_in_order_while_it_fits() {
    let e = state(vec![ticket(1, Side::Wsl, 2.0, 2, 10), ticket(2, Side::Wsl, 2.0, 2, 10), ticket(3, Side::Wsl, 2.0, 2, 10)]);
    let p = schedule(&e, &large(), &config(), H);
    assert_eq!(p.events, vec![Event::Cleared(1), Event::Cleared(2), Event::Cleared(3)]);
    let e = state(vec![
        ticket(1, Side::Wsl, 3.0, 2, 10),
        ticket(2, Side::Wsl, 3.0, 2, 10),
        ticket(3, Side::Wsl, 3.0, 2, 10),
    ]);
    let p = schedule(&e, &large(), &config(), H);
    assert_eq!(p.events, vec![Event::Cleared(1), Event::Cleared(2)]);
    assert_eq!(p.reasons[&3].decision, DecisionKind::Waiting);
    assert_eq!(p.reasons[&3].position, Some(1));
}

#[test]
fn backfill_of_a_ticket_ending_before_t() {
    let e = state(vec![
        active(ticket(1, Side::Wsl, 5.0, 2, 30), H - 60, 8.0),
        ticket(2, Side::Wsl, 3.0, 2, 10),
        ticket(3, Side::Wsl, 0.5, 1, 1),
    ]);
    let p = schedule(&e, &large(), &config(), H);
    // #3 uses the WSL RAM #2 lacks, but ends (1 min) before #2's estimated start (end of #1).
    assert_eq!(p.events, vec![Event::Cleared(3)]);
    let r2 = &p.reasons[&2];
    assert_eq!(r2.decision, DecisionKind::Waiting);
    assert_eq!(r2.position, Some(1));
    assert_eq!(r2.reserve.as_deref(), Some("ram_wsl"));
    assert_eq!(r2.blockers, vec![1]);
    assert_eq!(r2.unblocked_at, Some(H - 60 + 30 * 60));
    assert!(r2.message.contains("RAM WSL insuffisante"), "{}", r2.message);
    assert!(r2.message.contains("Débloqué à la fin du #1"), "{}", r2.message);
    assert!(r2.message.contains("Doublé par #3 (1/3)"), "{}", r2.message);
    let r3 = &p.reasons[&3];
    assert_eq!(r3.decision, DecisionKind::Cleared);
    assert!(r3.message.contains("passé devant #2 : finit avant son démarrage estimé"), "{}", r3.message);
    assert_eq!(p.state.ticket(2).unwrap().overtakes, 1);

    // A follower using the missing WSL RAM and ending after T stays behind, with the reason.
    let e = state(vec![
        active(ticket(1, Side::Wsl, 5.0, 2, 30), H - 60, 8.0),
        ticket(2, Side::Wsl, 3.0, 2, 10),
        ticket(3, Side::Wsl, 0.5, 1, 60),
    ]);
    let p = schedule(&e, &large(), &config(), H);
    assert!(p.events.is_empty());
    assert!(p.reasons[&3].message.contains("il utilise aussi la RAM WSL qui manque au #2"), "{}", p.reasons[&3].message);
}

/// #28 (Windows) was blocked "behind #26", which was waiting for the WSL RAM held by #24.
#[test]
fn backfill_real_case_11_05() {
    let c = Config::default();
    // As observed: 5.7 GB and 8 cores free on Windows (8.2 available − 1.5 margin − 1 GB of possible VM growth
    // for #24), WSL RAM exhausted.
    let m = measures(8.2, 3.0, 11.0);
    let e = state(vec![
        active(ticket(24, Side::Wsl, 4.0, 6, 60), H, 6.0),
        ticket(26, Side::Wsl, 3.0, 4, 15),
        ticket(28, Side::Windows, 4.0, 4, 5),
    ]);
    let p = schedule(&present(e, H + 120), &fresh(m, H + 120), &c, H + 120);
    assert_eq!(p.events, vec![Event::Cleared(28)], "{}", p.reasons[&28].message);
    // #26 lacks WSL RAM (held by #24) and WSL CPU (6 + 4 cores out of 10 − 2): #28 uses neither.
    assert!(
        p.reasons[&28].message.contains("passé devant #26 : n'utilise ni la RAM WSL ni le CPU WSL, qui lui manquent"),
        "{}",
        p.reasons[&28].message
    );
    // Different resource: it goes through without counting for anti-starvation.
    assert!(p.reasons[&26].message.contains("Passés sur une ressource dont il n'a pas besoin : #28"), "{}", p.reasons[&26].message);
    assert_eq!(p.state.ticket(26).unwrap().overtakes, 0);
}

#[test]
fn anti_starvation() {
    let c = config();
    let head = |overtakes: u32, created: u64| {
        let mut t = ticket(2, Side::Wsl, 3.0, 2, 10);
        t.overtakes = overtakes;
        t.created = created;
        t
    };
    // Uses the WSL RAM the head lacks, but ends (1 min) before T: counted pass.
    let conflict = || ticket(3, Side::Wsl, 0.5, 1, 1);
    let occupant = || active(ticket(1, Side::Wsl, 5.0, 2, 30), H - 60, 8.0);
    let p = schedule(&state(vec![occupant(), head(0, H), conflict()]), &large(), &c, H);
    assert_eq!(p.events, vec![Event::Cleared(3)]);
    assert_eq!(p.state.ticket(2).unwrap().overtakes, 1);
    // Already overtaken 3 times: no more "ends before T" passes.
    let p = schedule(&state(vec![occupant(), head(3, H), conflict()]), &large(), &c, H);
    assert!(p.events.is_empty());
    assert!(p.reasons[&3].message.contains("anti-famine : il utilise aussi la RAM WSL et #2 a déjà été doublé 3 fois"), "{}", p.reasons[&3].message);
    assert!(p.reasons[&2].message.contains("Anti-famine : plus aucun ticket ne passe devant"), "{}", p.reasons[&2].message);
    // Head waiting for 20 min: same.
    assert!(schedule(&state(vec![occupant(), head(0, H - 20 * 60), conflict()]), &large(), &c, H).events.is_empty());
    // Configurable: with 5 overtakes allowed, the 4th goes through.
    let c5 = Config { max_overtakes: 5, ..config() };
    assert_eq!(schedule(&state(vec![occupant(), head(3, H), conflict()]), &large(), &c5, H).events, vec![Event::Cleared(3)]);
    // A manual freeze stops any backfill.
    let mut e = state(vec![occupant(), head(0, H), ticket(3, Side::Windows, 1.0, 1, 5)]);
    e.manual_freeze = Some(H);
    assert!(schedule(&e, &large(), &c, H).events.is_empty());
}

/// Real case: #39 (Windows) blocked "behind #33 (anti-starvation)" while #33 (WSL) only waited for WSL RAM.
#[test]
fn anti_starvation_ignores_unrelated_resources() {
    let c = Config::default();
    let mut t33 = ticket(33, Side::Wsl, 4.0, 6, 30);
    t33.created = H - 55 * 60;
    t33.overtakes = 3;
    // #33 only lacks WSL RAM (3 GB available − 1.5 margin < 4 GB); Windows has 7.5 GB and 14 cores free.
    let e = state(vec![t33, ticket(39, Side::Windows, 3.0, 4, 10)]);
    let p = schedule(&present(e, H), &fresh(measures(9.0, 3.0, 7.0), H), &c, H);
    assert_eq!(p.events, vec![Event::Cleared(39)], "{}", p.reasons[&39].message);
    assert!(p.reasons[&39].message.contains("passé devant #33 : n'utilise pas la RAM WSL qui lui manque"), "{}", p.reasons[&39].message);
    assert_eq!(p.state.ticket(33).unwrap().overtakes, 3, "not counted");
}

#[test]
fn immediate_refusal_of_impossible_needs() {
    let c = config();
    let m = large();
    let r = refusal(Side::Wsl, &Need { ram_mb: 11 * 1024, cpu: 2, duration_s: 60 }, &m, &c).unwrap();
    assert_eq!(r.decision, DecisionKind::Refused);
    assert_eq!(r.reserve.as_deref(), Some("ram_wsl"));
    assert!(r.message.contains("RAM WSL impossible"), "{}", r.message);
    assert!(r.message.contains("--ram à 10.5G max"), "{}", r.message);

    // WSL: beyond 10 − 2 cores, admitted as exclusive; refused only beyond 10.
    assert!(refusal(Side::Wsl, &Need { ram_mb: 2048, cpu: 10, duration_s: 60 }, &m, &c).is_none());
    let r = refusal(Side::Wsl, &Need { ram_mb: 2048, cpu: 11, duration_s: 60 }, &m, &c).unwrap();
    assert_eq!(r.reserve.as_deref(), Some("cpu"));
    assert!(r.message.contains("--cpu à 10 max"), "{}", r.message);
    // Windows: its own budget of 16 logical cores.
    assert!(refusal(Side::Windows, &Need { ram_mb: 2048, cpu: 11, duration_s: 60 }, &m, &c).is_none());
    let r = refusal(Side::Windows, &Need { ram_mb: 2048, cpu: 17, duration_s: 60 }, &m, &c).unwrap();
    assert_eq!(r.reserve.as_deref(), Some("cpu_windows"));
    assert!(r.message.contains("--cpu à 16 max"), "{}", r.message);

    // 11 GB on Windows fit in 32 − 4 GB: WSL RAM does not count for a Windows task.
    assert!(refusal(Side::Windows, &Need { ram_mb: 11 * 1024, cpu: 2, duration_s: 60 }, &m, &c).is_none());
    assert!(refusal(Side::Windows, &Need { ram_mb: 29 * 1024, cpu: 2, duration_s: 60 }, &m, &c).is_some());
}

#[test]
fn wsl_ram_also_debits_windows_ram() {
    // Windows: 9 GB available − 4 GB margin = 5 GB; an active 4 GB WSL ticket reserves 4.
    let m = measures(9.0, 8.0, 4.0);
    let e = state(vec![active(ticket(1, Side::Wsl, 4.0, 2, 30), H - 60, 8.0), ticket(2, Side::Windows, 3.0, 2, 10)]);
    let p = schedule(&e, &m, &config(), H);
    assert!(p.events.is_empty());
    let r = &p.reasons[&2];
    assert_eq!(r.reserve.as_deref(), Some("ram_windows"));
    assert_eq!(r.reserved_by_active, Some(4096));

    let lev = levels(&p.state, &m, &config(), H);
    assert_eq!(lev[Reserve::RamWindows.i()].reserve, 4096);
    assert_eq!(lev[Reserve::RamWsl.i()].reserve, 4096);
    // A Windows task does not debit WSL RAM.
    let e = state(vec![active(ticket(1, Side::Windows, 4.0, 2, 30), H - 60, 8.0)]);
    let lev = levels(&e, &m, &config(), H);
    assert_eq!(lev[Reserve::RamWsl.i()].reserve, 0);
    assert_eq!(lev[Reserve::RamWindows.i()].reserve, 4096);
}

#[test]
fn wsl_ram_follows_memavailable_and_falls_back_on_vmmem() {
    let c = config();
    // vmmemWSL at 9.5 GB (cache included) but MemAvailable at 5.5 GB: MemAvailable is what counts.
    let m = measures(20.0, 5.5, 9.5);
    assert_eq!(available(Reserve::RamWsl, &m, &c, H), 5632);
    assert_eq!(available(Reserve::RamWsl, &m, &c, H + WSL_FRESHNESS_S), 5632);
    // Stale WSL measure (over 30 s): fall back on 12 GB − vmmemWSL.
    assert_eq!(available(Reserve::RamWsl, &m, &c, H + WSL_FRESHNESS_S + 1), 2560);
    // WSL stopped and no measure: the whole cap.
    let mut stopped = m.clone();
    stopped.vmmem = None;
    stopped.wsl = None;
    assert_eq!(available(Reserve::RamWsl, &stopped, &c, H), 12288);
    // A 3 GB WSL ticket goes through with MemAvailable, where the former min(…) rule would have made it wait.
    let e = state(vec![ticket(1, Side::Wsl, 3.0, 2, 10)]);
    assert_eq!(schedule(&e, &m, &c, H).events, vec![Event::Cleared(1)]);
}

#[test]
fn unattended_wait_skipped_after_90_s_then_removed_after_15_min() {
    let mut t = ticket(1, Side::Wsl, 9.0, 2, 10);
    t.listeners = 0;
    let mut e = state(vec![active(ticket(2, Side::Wsl, 5.0, 2, 30), H, 8.0), t, ticket(3, Side::Wsl, 1.0, 1, 5)]);
    e.ticket_mut(2).unwrap().presence = H + 89;
    let p = schedule(&e, &large(), &config(), H + 90);
    assert!(p.events.is_empty(), "#1 is still awaited, #3 stays behind");
    assert!(p.reasons[&3].message.contains("derrière #1"));

    let mut e2 = e.clone();
    e2.ticket_mut(2).unwrap().presence = H + 91;
    let p = schedule(&e2, &large(), &config(), H + 91);
    assert_eq!(p.events, vec![Event::Cleared(3)], "#1 without a client is skipped, its place is kept");
    let r1 = &p.reasons[&1];
    assert_eq!(r1.reserve.as_deref(), Some("absent"));
    assert_eq!(r1.position, Some(1));
    assert!(r1.message.contains("Relance « clearway wait 1 »"), "{}", r1.message);
    assert!(p.state.ticket(1).is_some());

    let mut e3 = e.clone();
    e3.ticket_mut(2).unwrap().presence = H + 15 * 60;
    let p = schedule(&e3, &large(), &config(), H + 15 * 60 + 1);
    assert!(p.events.contains(&Event::Expired(1)));

    // An open long-poll connection counts as presence.
    e.ticket_mut(1).unwrap().listeners = 1;
    let p = schedule(&e, &large(), &config(), H + 3000);
    assert!(!p.events.contains(&Event::Expired(1)));
}

#[test]
fn undelivered_clearance_recalled_after_2_min_then_skipped() {
    let e = state(vec![ticket(1, Side::Wsl, 1.0, 1, 10)]);
    let p = schedule(&e, &large(), &config(), H);
    assert_eq!(p.events, vec![Event::Cleared(1)]);
    let mut e = p.state;
    e.ticket_mut(1).unwrap().listeners = 0;
    let p = schedule(&e, &large(), &config(), H + 119);
    assert!(p.events.is_empty());
    assert!(p.reasons[&1].message.contains("pas encore accusé"), "{}", p.reasons[&1].message);
    let p = schedule(&e, &large(), &config(), H + 120);
    assert_eq!(p.events, vec![Event::Recalled(1)]);
    let t = p.state.ticket(1).unwrap();
    assert_eq!((t.state, t.granted, t.recalled), (TicketState::Waiting, None, Some(H + 120)));
    assert!(p.reasons[&1].message.contains("feu vert rappelé"), "{}", p.reasons[&1].message);

    // Once recalled, it does not take a clearance again by itself and lets the next ones through.
    let mut e = p.state;
    e.tickets.push(ticket(2, Side::Wsl, 1.0, 1, 10));
    e.ticket_mut(2).unwrap().presence = H + 130;
    let p = schedule(&e, &large(), &config(), H + 130);
    assert_eq!(p.events, vec![Event::Cleared(2)]);

    // Once delivered, it is never recalled.
    let mut e = state(vec![active(ticket(1, Side::Wsl, 1.0, 1, 10), H, 8.0)]);
    e.ticket_mut(1).unwrap().presence = H + 300;
    assert!(schedule(&e, &large(), &config(), H + 300).events.is_empty());
}

#[test]
fn manual_ticket_lease() {
    let mut t = active(ticket(1, Side::Wsl, 2.0, 2, 10), H, 8.0);
    t.manual = true;
    let e = state(vec![t, ticket(2, Side::Wsl, 1.0, 1, 5)]);
    // Without a supervisor, no orphan by silence before the deadline.
    let mut before = e.clone();
    before.ticket_mut(2).unwrap().presence = H + 599;
    let p = schedule(&before, &large(), &config(), H + 599);
    assert!(!p.events.iter().any(|ev| matches!(
        ev,
        Event::Orphaned(1) | Event::Lease(1) | Event::LeaseOverrun(1)
    )));
    assert!(p.reasons[&1].message.contains("Bail jusqu'à"), "{}", p.reasons[&1].message);

    // At the deadline: lease overrun, ticket still active and reserved, the queue goes on.
    let mut expired = e.clone();
    expired.ticket_mut(2).unwrap().presence = H + 600;
    let p = schedule(&expired, &large(), &config(), H + 600);
    assert_eq!(p.events, vec![Event::LeaseOverrun(1), Event::Cleared(2)]);
    let t = p.state.ticket(1).unwrap();
    assert_eq!((t.state, t.lease_overrun), (TicketState::Active, true));
    assert!(p.freeze_texts.is_empty(), "{:?}", p.freeze_texts);
    assert!(levels(&p.state, &large(), &config(), H + 600)[Reserve::RamWsl.i()].holders.contains(&1));
    let message = &p.reasons[&1].message;
    assert!(message.contains("Bail dépassé depuis 0 s"), "{message}");
    assert!(message.contains("clearway extend 1") && message.contains("clearway release 1"), "{message}");
    assert!(message.contains(&format!("orphelin à {}", hhmm(H + 600 + 900))), "{message}");

    // The event is given only once.
    let mut e = p.state;
    e.tickets.retain(|t| t.id != 2);
    assert!(schedule(&e, &large(), &config(), H + 1000).events.is_empty());

    // 15 min after the deadline without any news: orphaned, admissions frozen as before.
    let mut waiting = ticket(3, Side::Wsl, 1.0, 1, 5);
    waiting.presence = H + 1500;
    e.tickets.push(waiting);
    let p = schedule(&e, &large(), &config(), H + 1500);
    assert_eq!(p.events, vec![Event::Lease(1)]);
    let t = p.state.ticket(1).unwrap();
    assert_eq!((t.state, t.orphan_cause), (TicketState::Orphaned, Some(OrphanCause::Lease)));
    assert_eq!(p.reasons[&3].decision, DecisionKind::Frozen);
    assert!(p.reasons[&1].message.contains("bail échu depuis 15 min"), "{}", p.reasons[&1].message);
    assert!(p.reasons[&1].message.contains("clearway extend 1"), "{}", p.reasons[&1].message);

    // Never freed on measures, even with memory back for a long time.
    let mut e = p.state;
    for k in 0..10 {
        let at = H + 1600 + k * 30;
        e.ticket_mut(3).unwrap().presence = at;
        let p = schedule(&e, &fresh(large(), at), &config(), at);
        assert!(!p.events.iter().any(|ev| matches!(ev, Event::Freed(1, _))));
        e = p.state;
    }
}

#[test]
fn real_case_65_expired_lease_live_session_no_freeze() {
    // Real case: #65 (manual, wsl, 2 GB, 2 cores, 15 min) granted at 13:33:05, lease expired at 13:48:05 while the
    // session was still working; extended by 30 min at 14:13:33, done at 14:28:45. The queue was frozen meanwhile.
    let (c, m) = (config(), large());
    let grant = H;
    let deadline = grant + 15 * 60;
    let mut t65 = active(ticket(65, Side::Wsl, 2.0, 2, 15), grant, 8.0);
    t65.manual = true;

    let p = schedule(&state(vec![t65]), &m, &c, deadline + 1);
    assert_eq!(p.events, vec![Event::LeaseOverrun(65)]);
    assert!(p.freeze_texts.is_empty(), "{:?}", p.freeze_texts);
    assert_eq!(p.reasons[&65].decision, DecisionKind::Active);

    // Another session asks for a WSL slot during the overrun: it goes through on what is left.
    let mut e = p.state;
    let mut t66 = ticket(66, Side::Wsl, 3.0, 4, 10);
    t66.presence = deadline + 60;
    e.tickets.push(t66);
    let p = schedule(&e, &m, &c, deadline + 60);
    assert_eq!(p.events, vec![Event::Cleared(66)]);
    let mut e = p.state;
    e.tickets.retain(|t| t.id != 66);

    // The session shows up at 14:00:05 (`clearway wait 65`): the orphan deadline moves back accordingly.
    e.ticket_mut(65).unwrap().presence = grant + 27 * 60;
    let p = schedule(&e, &m, &c, grant + 40 * 60);
    assert!(p.events.is_empty(), "{:?}", p.events);
    assert_eq!(state_of(&p.state, 65), Some(TicketState::Active));
    assert!(p.freeze_texts.is_empty(), "{:?}", p.freeze_texts);
    let message = &p.reasons[&65].message;
    assert!(message.contains("Bail dépassé depuis 25 min"), "{message}");
    assert!(message.contains(&format!("orphelin à {}", hhmm(grant + 42 * 60))), "{message}");

    // Extended by 30 min at 14:13:33: lease running again.
    let mut e = p.state;
    let extend_cmd = e.ticket_mut(65).unwrap();
    extend_cmd.need.duration_s = 40 * 60 + 28 + 30 * 60;
    extend_cmd.lease_overrun = false;
    extend_cmd.presence = grant + 40 * 60 + 28;
    let p = schedule(&e, &m, &c, grant + 41 * 60);
    assert!(p.events.is_empty(), "{:?}", p.events);
    assert!(p.reasons[&65].message.contains("Bail jusqu'à"), "{}", p.reasons[&65].message);
}

#[test]
fn manual_freeze() {
    let mut e = state(vec![ticket(1, Side::Windows, 1.0, 1, 5), ticket(2, Side::Windows, 1.0, 1, 5)]);
    e.manual_freeze = Some(H - 60);
    let p = schedule(&e, &large(), &config(), H);
    assert!(p.events.is_empty());
    let r = &p.reasons[&1];
    assert_eq!(r.decision, DecisionKind::Frozen);
    assert!(r.message.contains("gel manuel depuis"), "{}", r.message);
    assert!(p.reasons[&2].message.contains("derrière #1"));
    assert!(p.freeze_texts.iter().any(|g| g.contains("gel manuel")));
    e.manual_freeze = None;
    assert_eq!(schedule(&e, &large(), &config(), H).events.len(), 2);
}

#[test]
fn silent_active_becomes_orphan_and_freezes_admissions() {
    let e = state(vec![active(ticket(1, Side::Wsl, 1.0, 1, 30), H, 8.0), ticket(2, Side::Wsl, 1.0, 1, 5)]);
    let silent = H + config().silence_delay_s + 1;
    let p = schedule(&e, &large(), &config(), silent);
    assert_eq!(p.events, vec![Event::Orphaned(1)]);
    assert_eq!(state_of(&p.state, 1), Some(TicketState::Orphaned));
    let r2 = &p.reasons[&2];
    assert_eq!(r2.decision, DecisionKind::Frozen);
    assert_eq!(r2.blockers, vec![1]);
    assert!(r2.message.contains("ticket #1 orphelin"), "{}", r2.message);
    assert!(r2.message.contains("« clearway cancel 1 »"), "{}", r2.message);
    assert_eq!(p.reasons[&1].decision, DecisionKind::Orphaned);
    assert!(!p.freeze_texts.is_empty());

    // The orphan's reservation still counts.
    let lev = levels(&p.state, &large(), &config(), silent);
    assert_eq!(lev[Reserve::RamWsl.i()].reserve, 1024);
}

#[test]
fn orphan_never_freed_on_measures() {
    let c = config();
    let mut e = state(vec![active(ticket(1, Side::Wsl, 3.0, 2, 30), H, 6.0)]);
    e.ticket_mut(1).unwrap().state = TicketState::Orphaned;
    e.ticket_mut(1).unwrap().orphan_cause = Some(OrphanCause::Silence);
    for k in 0..20 {
        let at = H + 100 + k * 30;
        let p = schedule(&e, &fresh(measures(20.0, 8.0, 4.0), at), &c, at);
        assert!(p.events.is_empty(), "{:?}", p.events);
        e = p.state;
    }
    assert_eq!(e.ticket(1).unwrap().state, TicketState::Orphaned);
}

#[test]
fn wsl_stop_frees_only_wsl_tickets() {
    let e = state(vec![
        active(ticket(1, Side::Wsl, 2.0, 2, 30), H, 8.0),
        active(ticket(2, Side::Windows, 2.0, 2, 30), H, 20.0),
        ticket(3, Side::Wsl, 1.0, 1, 5),
    ]);
    let mut m = large();
    m.vmmem = None;
    m.wsl = None;
    let p = schedule(&e, &m, &config(), H + 5);
    assert_eq!(
        p.events,
        vec![Event::Freed(1, FreeReason::WslStopped), Event::Freed(3, FreeReason::WslStopped)]
    );
    assert_eq!(state_of(&p.state, 2), Some(TicketState::Active));
    assert_eq!(p.state.vmmem_pid, None);

    // WSL restart: new vmmemWSL, no WSL ticket survives it.
    let mut m2 = large();
    m2.vmmem = Some(Vmmem { pid: 999, rss_mb: 1024 });
    let p = schedule(&e, &m2, &config(), H + 5);
    assert!(p.state.tickets.iter().all(|t| t.side == Side::Windows));
}

#[test]
fn windows_ram_pressure_freezes_and_explains() {
    let e = state(vec![ticket(1, Side::Windows, 1.0, 1, 5)]);
    let p = schedule(&e, &measures(3.0, 8.0, 4.0), &config(), H);
    assert!(p.events.is_empty());
    assert_eq!(p.pressure, vec![Reserve::RamWindows]);
    let r = &p.reasons[&1];
    assert_eq!(r.decision, DecisionKind::Frozen);
    assert!(r.message.contains("pression : RAM Windows disponible 3 Go sous la marge de 4 Go"), "{}", r.message);
}

#[test]
fn wsl_pressure_does_not_freeze_a_windows_task() {
    let e = state(vec![ticket(1, Side::Windows, 1.0, 1, 5)]);
    let p = schedule(&e, &measures(20.0, 1.0, 4.0), &config(), H);
    assert_eq!(p.pressure, vec![Reserve::RamWsl]);
    assert_eq!(p.events, vec![Event::Cleared(1)]);
}

#[test]
fn room_taken_outside_queue_and_suggestion() {
    // No active ticket: only memory used elsewhere blocks; 3 GB − 1.5 GB margin = 1.5 GB free.
    let e = state(vec![ticket(1, Side::Wsl, 2.0, 2, 10)]);
    let p = schedule(&e, &measures(20.0, 3.0, 4.0), &config(), H);
    let r = &p.reasons[&1];
    assert_eq!(r.unblocked_at, None);
    assert!(r.message.contains("la place est prise hors file"), "{}", r.message);
    assert!(r.message.contains("--ram 1.5G ou moins"), "{}", r.message);
}

#[test]
fn key_ignores_measured_values() {
    let e = state(vec![active(ticket(1, Side::Wsl, 5.0, 2, 30), H, 8.0), ticket(2, Side::Wsl, 3.0, 2, 10)]);
    let a = schedule(&e, &measures(20.0, 8.0, 4.0), &config(), H);
    let b = schedule(&e, &measures(19.0, 7.5, 4.2), &config(), H + 2);
    assert_ne!(a.reasons[&2].message, b.reasons[&2].message);
    assert_eq!(a.reasons[&2].key(), b.reasons[&2].key());
}

#[test]
fn nothing_to_persist_without_decision() {
    let e = state(vec![active(ticket(1, Side::Wsl, 2.0, 2, 30), H, 8.0)]);
    let p = schedule(&e, &large(), &config(), H + 10);
    assert!(!p.persist);
}

/// Wrongly refused before the Windows debit was fixed.
#[test]
fn real_case_11_05_wsl_ticket_of_3_gb() {
    let c = Config::default();
    let m = measures(4.7, 6.0, 10.8);
    let e = state(vec![ticket(1, Side::Wsl, 3.0, 2, 5)]);
    let p = schedule(&e, &m, &c, H);
    assert_eq!(p.events, vec![Event::Cleared(1)], "{}", p.reasons[&1].message);
    // The VM can only grow by 1.2 GB more: that is all the ticket reserves on Windows RAM.
    let lev = levels(&p.state, &m, &c, H);
    assert_eq!(lev[Reserve::RamWindows.i()].reserve, 1229);
    assert_eq!(lev[Reserve::RamWsl.i()].reserve, 3072);
    // A Windows ticket of the same size debits its whole need: 4.7 − 1.2 − 1.5 = 2 GB free, it waits.
    let e = state(vec![active(ticket(1, Side::Wsl, 3.0, 2, 5), H, 6.0), ticket(2, Side::Windows, 3.0, 2, 5)]);
    let p = schedule(&e, &m, &c, H);
    assert!(p.events.is_empty());
    assert_eq!(p.reasons[&2].reserve.as_deref(), Some("ram_windows"));
}

#[test]
fn windows_debit_of_a_wsl_ticket() {
    let c = Config::default();
    let b = Need { ram_mb: 3072, cpu: 2, duration_s: 60 };
    assert_eq!(debit(Reserve::RamWindows, Side::Wsl, &b, &measures(20.0, 8.0, 4.0), &c), 3072);
    assert_eq!(debit(Reserve::RamWindows, Side::Wsl, &b, &measures(20.0, 8.0, 11.0), &c), 1024);
    assert_eq!(debit(Reserve::RamWindows, Side::Wsl, &b, &measures(20.0, 8.0, 12.5), &c), 0);
    assert_eq!(debit(Reserve::RamWindows, Side::Windows, &b, &measures(20.0, 8.0, 12.5), &c), 3072);
    let mut halted = measures(20.0, 8.0, 4.0);
    halted.vmmem = None;
    assert_eq!(debit(Reserve::RamWindows, Side::Wsl, &b, &halted, &c), 3072);
}

#[test]
fn no_wsl_pressure_without_recent_measure() {
    let c = Config::default();
    // vmmemWSL at 10.9 GB: the fallback gives 1.1 GB, below the margin, but it is only assumed cache.
    let m = measures(20.0, 6.0, 10.9);
    assert!(schedule(&state(vec![]), &m, &c, H + WSL_FRESHNESS_S + 1).pressure.is_empty());
    assert_eq!(schedule(&state(vec![]), &fresh(measures(20.0, 1.0, 10.9), H), &c, H).pressure, vec![Reserve::RamWsl]);
}

#[test]
fn exclusive_cpu_request_runs_alone() {
    let c = config();
    let e = state(vec![active(ticket(1, Side::Wsl, 1.0, 1, 10), H, 8.0), ticket(2, Side::Wsl, 3.0, 10, 30)]);
    let p = schedule(&e, &large(), &c, H + 5);
    assert!(p.events.is_empty());
    let r = &p.reasons[&2];
    assert_eq!(r.reserve.as_deref(), Some("exclusive"));
    assert_eq!(r.blockers, vec![1]);
    assert_eq!(r.unblocked_at, Some(H + 600));
    assert!(r.message.contains("demande exclusive"), "{}", r.message);

    // An active Windows ticket does not block WSL exclusivity; then the exclusive one blocks only other WSL tickets.
    let e = state(vec![
        active(ticket(1, Side::Windows, 1.0, 0, 10), H, 20.0),
        ticket(2, Side::Wsl, 3.0, 10, 30),
        ticket(3, Side::Wsl, 1.0, 1, 5),
    ]);
    let p = schedule(&e, &large(), &c, H + 5);
    assert_eq!(p.events, vec![Event::Cleared(2)]);
    assert_eq!(p.reasons[&3].reserve.as_deref(), Some("cpu"));
}

/// #13 (Windows, 4 cores) waited for "insufficient CPU" behind #12 (WSL, 10 cores).
#[test]
fn separate_windows_cpu_budget() {
    let c = config();
    let e = state(vec![
        active(ticket(12, Side::Wsl, 3.0, 10, 30), H, 8.0),
        ticket(13, Side::Windows, 2.0, 4, 10),
        ticket(14, Side::Windows, 1.0, 1, 10),
    ]);
    let p = schedule(&e, &large(), &c, H + 5);
    // Windows: 16 − 10 (vCPUs of #12) − 2 = 4 free, just enough for #13; #14 waits next.
    assert_eq!(p.events, vec![Event::Cleared(13)]);
    assert_eq!(p.reasons[&14].reserve.as_deref(), Some("cpu_windows"));
    let lev = levels(&p.state, &large(), &c, H + 5);
    assert_eq!(lev[Reserve::Cpu.i()].reserve, 10, "the WSL budget only sees #12");
    assert_eq!(lev[Reserve::CpuWindows.i()].reserve, 14);

    // `cpu.total_windows` replaces the measure.
    let c8 = Config { cpu_total_windows: Some(8), ..config() };
    let p = schedule(&e, &large(), &c8, H + 5);
    assert!(p.events.is_empty());
    assert_eq!(p.reasons[&13].reserve.as_deref(), Some("cpu_windows"));
}

/// Replay of the prototype's log. Needs, durations and measures are ASSUMED from the descriptions:
/// the log gives neither peaks nor reserves, it is not an oracle.
#[test]
fn evening_of_11_04() {
    let c = config();
    // Assumed: 14 GB free on Windows, vmmemWSL at 6 GB, MemAvailable 6 GB.
    let m = measures(14.0, 6.0, 6.0);
    let t23_35 = H - 11 * 60;
    let t23_46 = H;
    let t23_56 = H + 10 * 60;
    let t00_00 = H + 14 * 60;

    // #3 (targeted Infection, 1 GB, 1 core) occupies WSL; #4 (back.sh then Infection --threads=max, 10 cores)
    // arrives at 23:35: admitted, but only when alone on WSL.
    assert!(refusal(Side::Wsl, &Need { ram_mb: 3072, cpu: 10, duration_s: 45 * 60 }, &m, &c).is_none());
    let t3 = active(ticket(3, Side::Wsl, 1.0, 1, 15), t23_35 - 2 * 60, 6.0);
    let mut t4 = ticket(4, Side::Wsl, 3.0, 10, 45);
    t4.created = t23_35;
    let p = schedule(&present(state(vec![t3, t4.clone()]), t23_35), &m, &c, t23_35);
    assert!(p.events.is_empty());
    assert_eq!(p.reasons[&4].reserve.as_deref(), Some("exclusive"));
    // #3 ends at 23:46: #4 goes through alone.
    let mut e = p.state;
    e.tickets.retain(|t| t.id != 3);
    let p = schedule(&present(e, t23_46), &m, &c, t23_46);
    assert_eq!(p.events, vec![Event::Cleared(4)]);

    // During #4 (10 cores reserved), #5 (targeted Infection) waits for the CPU.
    let mut e = p.state;
    e.ticket_mut(4).unwrap().acked = true;
    e.tickets.push(ticket(5, Side::Wsl, 1.0, 1, 5));
    let p = schedule(&present(e, t23_56), &m, &c, t23_56);
    assert!(p.events.is_empty());
    assert_eq!(p.reasons[&5].reserve.as_deref(), Some("cpu"));
    assert_eq!(p.reasons[&5].unblocked_at, Some(t23_46 + 45 * 60));

    // #6 (Godot, 3 GB WSL) stays behind; #7 (Blender, Windows) goes ahead of #5, as was done by hand with the prototype.
    let mut e = p.state;
    e.tickets.push(ticket(6, Side::Wsl, 3.0, 2, 15));
    e.tickets.push(ticket(7, Side::Windows, 3.0, 2, 6));
    let p = schedule(&present(e, t00_00), &m, &c, t00_00);
    assert_eq!(p.events, vec![Event::Cleared(7)]);
    assert!(p.reasons[&7].message.contains("passé devant #5 : n'utilise pas le CPU WSL qui lui manque"), "{}", p.reasons[&7].message);
    assert_eq!(p.reasons[&6].position, Some(2));
    assert!(p.reasons[&6].message.contains("derrière #5"));

    // #4 ends: #5 and #6 start in the same pass.
    let mut e = p.state;
    e.tickets.retain(|t| t.id != 4);
    let p = schedule(&present(e, t00_00 + 60), &m, &c, t00_00 + 60);
    assert_eq!(p.events, vec![Event::Cleared(5), Event::Cleared(6)]);

    // #23 (same request as #4): alone on WSL, it goes through at once.
    let mut t23 = ticket(23, Side::Wsl, 3.0, 10, 45);
    t23.created = H;
    let p = schedule(&state(vec![active(ticket(22, Side::Windows, 5.0, 4, 20), H, 14.0), t23]), &m, &c, H + 1);
    assert_eq!(p.events, vec![Event::Cleared(23)]);
}

#[test]
fn free_display_never_negative() {
    let c = config();
    let e = state(vec![active(ticket(18, Side::Wsl, 3.0, 10, 30), H, 8.0)]);
    let views = reserve_views(&e, &large(), &c, H);
    let cpu = views.iter().find(|v| v.key == "cpu").unwrap();
    assert_eq!(cpu.free, -2, "the computation stays exact");
    assert_eq!(cpu.exclusive, Some(18));
    assert_eq!(cpu.free_text(), "0 cœur (exclusif #18)");
    let win = views.iter().find(|v| v.key == "cpu_windows").unwrap();
    assert_eq!((win.exclusive, win.free_text()), (None, "4 cœurs".to_owned()));
    // RAM reserved beyond what is available: shown as 0, without an exclusive mention.
    let e = state(vec![active(ticket(1, Side::Windows, 18.0, 1, 30), H, 20.0)]);
    let ram = reserve_views(&e, &large(), &c, H).into_iter().find(|v| v.key == "ram_windows").unwrap();
    assert!(ram.free < 0);
    assert_eq!(ram.free_text(), "0 Go");
}

/// Swap at 91%: pressure only if it really pages out or memory is short, with hysteresis.
#[test]
fn full_but_calm_swap_does_not_freeze() {
    let c = Config::default();
    let e = state(vec![ticket(1, Side::Wsl, 1.0, 1, 5)]);
    let swap = |avail_gb: f64, free_mb: u64, out: f32, t: u64| {
        let mut m = fresh(measures(20.0, avail_gb, 6.0), t);
        m.wsl.as_mut().unwrap().swap_free_mb = free_mb;
        m.swap_out_ps = out;
        m
    };
    // Real case: full swap, almost no paging out, MemAvailable 5.2 GB: no pressure nor freeze, just information.
    let calm = swap(5.2, 377, 2.0, H);
    let p = schedule(&e, &calm, &c, H);
    assert!(p.pressure.is_empty());
    assert_eq!(p.events, vec![Event::Cleared(1)]);
    assert_eq!(calm_swap(&p.state, &calm, &c, H).as_deref(), Some("swap WSL plein à 90 % (calme)"));

    // Full swap paging out at 300 pages/s: pressure, WSL tickets frozen, explicit reason.
    let p = schedule(&e, &swap(5.2, 377, 300.0, H), &c, H);
    assert_eq!(p.pressure, vec![Reserve::RamWsl]);
    assert!(p.reasons[&1].message.contains("swap WSL plein à 90 % et actif (sortie 300 pages/s"), "{}", p.reasons[&1].message);
    // Calm afterwards: pressure holds for 59 s, then lifts at 60 s.
    let mut e2 = p.state;
    for k in 0..=59 {
        e2 = schedule(&e2, &swap(5.2, 377, 0.0, H + 1 + k), &c, H + 1 + k).state;
        assert!(e2.swap_pressure, "lifted too early after {} s of calm", k);
    }
    assert!(!schedule(&e2, &swap(5.2, 377, 0.0, H + 61), &c, H + 61).state.swap_pressure);
    // Below 85%: lifted at once.
    let p = schedule(&e, &swap(5.2, 377, 300.0, H), &c, H);
    assert!(!schedule(&p.state, &swap(5.2, 700, 300.0, H + 2), &c, H + 2).state.swap_pressure);
    // Full swap and MemAvailable below margin + 1 GB (2.5 GB): pressure even without paging out.
    assert_eq!(schedule(&e, &swap(2.2, 377, 0.0, H), &c, H).pressure, vec![Reserve::RamWsl]);
}

#[test]
fn forcing_outside_queue_reserves_and_freeze() {
    let c = Config::default();
    let m = measures(5.2, 3.0, 9.0);
    let mut e = state(vec![
        active(ticket(24, Side::Wsl, 4.0, 6, 60), H, 6.0),
        ticket(26, Side::Wsl, 3.0, 4, 15),
        ticket(28, Side::Windows, 4.0, 4, 5),
        ticket(29, Side::Windows, 1.0, 1, 5),
    ]);
    e.manual_freeze = Some(H);
    e.ticket_mut(26).unwrap().overtakes = 1;
    let preview = force_preview(&e, 28, &m, &c, H).unwrap();
    assert!(preview.starts_with("Forcer #28 (windows, 4 Go, 4 cœurs) ?"), "{preview}");
    // Windows RAM: 5.2 − 3 (possible VM growth for #24) − 1.5 = 0.7 GB free, − 4 GB = −3.3 GB.
    assert!(preview.contains("RAM Windows libre après : -3,3 Go (marge 1,5 Go)"), "{preview}");
    assert!(preview.contains("CPU Windows libre après : 4 cœurs (marge 2 cœurs)"), "{preview}");
    assert!(!preview.contains("RAM WSL"), "a Windows ticket does not debit WSL: {preview}");

    let (forced, log) = force_through(&e, 28, &m, &c, H).unwrap();
    assert!(log.contains("#28 (windows, 4 Go, 4 cœurs) · réserves au moment du forçage"), "{log}");
    let t = forced.ticket(28).unwrap();
    assert_eq!((t.state, t.force), (TicketState::Active, true));
    assert_eq!(forced.ticket(26).unwrap().overtakes, 1, "forcing does not count as an overtake");
    // It counts in the reservations; the others keep their place and the freeze still holds for them.
    let p = schedule(&forced, &m, &c, H);
    assert!(p.events.is_empty());
    assert_eq!(levels(&p.state, &m, &c, H)[Reserve::RamWindows.i()].reserve, 3072 + 4096);
    assert!(p.reasons[&28].message.contains("actif (forcé à la main)"), "{}", p.reasons[&28].message);
    assert_eq!(p.reasons[&26].position, Some(1));
    assert_eq!(p.reasons[&29].position, Some(2));
    // Nothing to force on an active or unknown ticket.
    assert!(force_through(&forced, 28, &m, &c, H).is_err());
    assert!(force_preview(&e, 99, &m, &c, H).is_none());
}

/// #43 (Windows, 4 GB) waited behind the reservation of #42 (WSL), whose usage was already in the measures.
#[test]
fn reservation_limited_to_rampup() {
    let c = Config::default();
    let granted = H - 10 * 60;
    let mut t42 = active(ticket(42, Side::Wsl, 4.0, 10, 60), granted, 7.0);
    t42.base_vmmem_mb = Some(7 * 1024);
    t42.base_ram_wsl_mb = Some(7 * 1024);
    t42.base_ram_windows_mb = Some(10 * 1024);
    // Since its clearance, vmmemWSL went from 7 to 10.1 GB and MemAvailable from 7 to 3 GB: it took its 4 GB.
    let m = fresh(measures(6.4, 3.0, 10.1), H);
    let e = present(state(vec![t42.clone(), ticket(43, Side::Windows, 4.0, 4, 10)]), H);
    let p = schedule(&e, &m, &c, H);
    assert_eq!(p.events, vec![Event::Cleared(43)], "{}", p.reasons[&43].message);
    assert_eq!(reservation(&State::default(), &t42, Reserve::RamWindows, &m, &c, H), 0);
    assert_eq!(reservation(&State::default(), &t42, Reserve::RamWsl, &m, &c, H), 0);
    assert_eq!(reservation(&State::default(), &t42, Reserve::Cpu, &m, &c, H), 1, "measured load 1.0: one core left reserved");
    assert!(
        p.reasons[&42].message.contains("Montée en charge terminée, réservé encore : RAM Windows 0 Go, CPU Windows 1 cœur, RAM WSL 0 Go, CPU WSL 1 cœur"),
        "{}",
        p.reasons[&42].message
    );

    // During the first 3 minutes: whole reservation (Windows debit 1.9 GB = 12 − 10.1), #43 waits.
    let mut young = t42.clone();
    young.granted = Some(H - 60);
    let e = present(state(vec![young.clone(), ticket(43, Side::Windows, 4.0, 4, 10)]), H);
    let p = schedule(&e, &m, &c, H);
    assert!(p.events.is_empty());
    assert!(p.reasons[&43].message.contains("dont 1,9 Go réservés pour #42"), "{}", p.reasons[&43].message);
    assert!(p.reasons[&42].message.contains("Montée en charge jusqu'à"), "{}", p.reasons[&42].message);

    // Partial ramp-up: 1.5 GB taken out of 4 GB declared → 2.5 GB still reserved on WSL.
    let m2 = fresh(measures(9.0, 5.5, 8.5), H);
    assert_eq!(reservation(&State::default(), &t42, Reserve::RamWsl, &m2, &c, H), 2560);

    // Ticket granted by an older exe (no baselines): fall back on the available RAM recorded at grant time.
    let mut old = t42.clone();
    old.base_vmmem_mb = None;
    old.base_ram_wsl_mb = None;
    old.base_ram_windows_mb = None;
    old.avail_at_grant_mb = Some(7 * 1024);
    assert_eq!(reservation(&State::default(), &old, Reserve::RamWsl, &m, &c, H), 0);
    assert_eq!(reservation(&State::default(), &old, Reserve::RamWindows, &m, &c, H), 0);

    // An orphan keeps its whole reservation.
    let mut orphan = t42.clone();
    orphan.state = TicketState::Orphaned;
    assert_eq!(reservation(&State::default(), &orphan, Reserve::RamWsl, &m, &c, H), 4096);
}

#[test]
fn wsl_ram_fallback_shown_as_estimate() {
    let e = state(Vec::new());
    let c = config();
    let ram_wsl = |m: &Measures, at: u64| reserve_views(&e, m, &c, at).into_iter().find(|r| r.key == "ram_wsl").unwrap();

    let fresh = ram_wsl(&large(), H + 10);
    assert_eq!(fresh.estimate, None);
    assert!(!fresh.free_text().contains("estimation"), "{}", fresh.free_text());

    // Stale WSL measure: 12 GB − vmmemWSL fallback, cache included, never shown as a measured free amount.
    let mut full = large();
    full.vmmem = Some(Vmmem { pid: 4242, rss_mb: 12 * 1024 });
    let r = ram_wsl(&full, H + 120);
    assert_eq!(r.estimate, Some(H));
    let text = r.free_text();
    assert!(text.starts_with("estimation 0 Go, cache compris"), "{text}");
    assert!(text.contains(&format!("pas de mesure WSL depuis {}", hhmm(H))), "{text}");

    full.wsl = None;
    assert!(ram_wsl(&full, H).free_text().ends_with("aucune mesure WSL"));
    full.vmmem = None;
    assert_eq!(ram_wsl(&full, H).estimate, None, "WSL stopped: no estimate");
}

#[test]
fn lost_link_resume_or_release() {
    let c = config();
    let mut linked_active = active(ticket(1, Side::Wsl, 2.0, 2, 30), H, 8.0);
    linked_active.manual = true;
    linked_active.link = Some(5);
    linked_active.lost = Some(H + 100);
    let mut linked_waiting = ticket(2, Side::Wsl, 1.0, 1, 10);
    linked_waiting.link = Some(5);
    linked_waiting.lost = Some(H + 100);
    let mut other = ticket(3, Side::Wsl, 1.0, 1, 10);
    other.presence = H + 110;
    let e = state(vec![linked_active, linked_waiting, other]);

    // Within the resume delay: the active one becomes a "connection" orphan without freezing, the waiting one is
    // skipped, the others go through.
    let p = schedule(&e, &large(), &c, H + 110);
    assert_eq!(p.events, vec![Event::Cleared(3)]);
    let t = p.state.ticket(1).unwrap();
    assert_eq!((t.state, t.orphan_cause), (TicketState::Orphaned, Some(OrphanCause::Connection)));
    assert!(p.freeze_texts.is_empty(), "{:?}", p.freeze_texts);
    assert!(levels(&p.state, &large(), &c, H + 110)[Reserve::RamWsl.i()].holders.contains(&1), "reservation kept");
    let message = &p.reasons[&1].message;
    assert!(message.contains("liaison de sa session perdue") && message.contains("sans geler"), "{message}");
    assert!(message.contains(&format!("libéré à {}", hhmm(H + 220))), "{message}");
    let message = &p.reasons[&2].message;
    assert_eq!(p.reasons[&2].reserve.as_deref(), Some("absent"));
    assert!(message.contains(&format!("place gardée jusqu'à {}", hhmm(H + 220))), "{message}");

    // Past the resume delay without a new link: both leave the queue.
    let p = schedule(&p.state, &large(), &c, H + 220);
    assert_eq!(
        p.events,
        vec![Event::Freed(1, FreeReason::SessionClosed), Event::Freed(2, FreeReason::SessionClosed)]
    );
    assert!(p.state.ticket(1).is_none() && p.state.ticket(2).is_none() && p.state.ticket(3).is_some());
}

#[test]
fn reasons_in_english() {
    let e = state(vec![ticket(1, Side::Wsl, 3.0, 2, 10), ticket(2, Side::Wsl, 3.0, 2, 10), ticket(3, Side::Wsl, 3.0, 2, 10)]);
    let p = crate::i18n::with(crate::i18n::Lang::En, || schedule(&e, &large(), &config(), H));
    assert_eq!(p.events, vec![Event::Cleared(1), Event::Cleared(2)]);
    let message = &p.reasons[&3].message;
    assert!(message.starts_with("#3 waiting, position 1: WSL RAM insufficient (need "), "{message}");
    assert!(message.contains("Unblocked when"), "{message}");
    assert!(p.reasons[&1].message.starts_with("#1: cleared. Free at clearance: "), "{}", p.reasons[&1].message);
}

#[test]
fn presence_relay_late_by_100_s_is_not_an_orphan() {
    let c = config();
    assert_eq!(c.silence_delay_s, 150);
    let t = active(ticket(1, Side::Windows, 1.0, 1, 30), H, 8.0);
    let p = schedule(&state(vec![t]), &large(), &c, H + 100);
    assert_eq!(state_of(&p.state, 1), Some(TicketState::Active), "a presence 100 s late is no orphan");
    let p = schedule(&p.state, &large(), &c, H + 151);
    assert_eq!(p.events, vec![Event::Orphaned(1)]);
}

#[test]
fn measured_cpu_pressure_freezes_only_that_side() {
    let c = config();
    let saturated = |load1: f32, at: u64| {
        let mut m = fresh(large(), at);
        if let Some(w) = &mut m.wsl {
            w.load1 = load1;
        }
        m
    };
    // Load 11.2 on 10 cores: pressure declared after 60 s, not before.
    let mut e = state(vec![]);
    for k in 0..=6 {
        let at = H + k * 10;
        e = schedule(&e, &saturated(11.2, at), &c, at).state;
    }
    assert!(e.cpu_pressure.wsl.on);
    let mut wsl = ticket(1, Side::Wsl, 1.0, 2, 10);
    wsl.presence = H + 70;
    let mut windows = ticket(2, Side::Windows, 1.0, 2, 10);
    windows.presence = H + 70;
    e.tickets = vec![wsl, windows];
    let p = schedule(&e, &saturated(11.2, H + 70), &c, H + 70);
    assert_eq!(p.events, vec![Event::Cleared(2)], "the Windows ticket does not take WSL cores");
    let message = &p.reasons[&1].message;
    assert!(message.contains("CPU WSL saturé (load 11,2 / 10 cœurs) depuis 1 min"), "{message}");
    assert_eq!(reserve_views(&p.state, &saturated(11.2, H + 70), &c, H + 70)[3].used_pct, Some(112));

    // Under 70 % for 60 s: lifted (hysteresis: 80 % keeps it on).
    let mut e = p.state;
    e.tickets.retain(|t| t.id == 1);
    for k in 0..=6 {
        let at = H + 80 + k * 10;
        e.ticket_mut(1).unwrap().presence = at;
        e = schedule(&e, &saturated(8.0, at), &c, at).state;
    }
    assert!(e.cpu_pressure.wsl.on, "80 % is above the release threshold");
    let mut lifted = false;
    for k in 0..=7 {
        let at = H + 160 + k * 10;
        e.ticket_mut(1).unwrap().presence = at;
        let p = schedule(&e, &saturated(5.0, at), &c, at);
        lifted |= p.events.contains(&Event::Cleared(1));
        e = p.state;
    }
    assert!(lifted && !e.cpu_pressure.wsl.on);
}

#[test]
fn settled_cpu_follows_the_measured_load() {
    let c = config();
    let mut m = fresh(large(), H);
    m.wsl.as_mut().unwrap().load1 = 3.2;
    m.cpu_pct = 50.0;
    let t1 = active(ticket(1, Side::Wsl, 1.0, 6, 60), H - 600, 8.0);
    let t2 = active(ticket(2, Side::Wsl, 1.0, 2, 60), H - 600, 8.0);
    let young = active(ticket(3, Side::Wsl, 1.0, 2, 60), H - 60, 8.0);
    let e = present(state(vec![t1.clone(), t2.clone(), young.clone()]), H);
    // Load 3.2 shared 6:2 between the settled ones; the young one keeps its whole declaration.
    assert_eq!(reservation(&e, &t1, Reserve::Cpu, &m, &c, H), 3);
    assert_eq!(reservation(&e, &t2, Reserve::Cpu, &m, &c, H), 1);
    assert_eq!(reservation(&e, &young, Reserve::Cpu, &m, &c, H), 2);
    // Windows: 50 % of 16 cores = 8 busy, more than the 8 declared by the settled ones: capped at the declaration.
    assert_eq!(reservation(&e, &t1, Reserve::CpuWindows, &m, &c, H), 6);
    // Without a fresh WSL measure: whole declaration.
    let stale = fresh(large(), H - 120);
    assert_eq!(reservation(&e, &t1, Reserve::Cpu, &stale, &c, H), 6);
    // Idle: the floor stays reserved.
    let mut idle = m.clone();
    idle.wsl.as_mut().unwrap().load1 = 0.0;
    assert_eq!(reservation(&e, &t2, Reserve::Cpu, &idle, &c, H), CPU_FLOOR);
}

#[test]
fn exclusive_goes_when_the_measured_load_allows() {
    let c = config();
    let mut m = fresh(large(), H);
    m.wsl.as_mut().unwrap().load1 = 0.4;
    // #1 settled and nearly idle: 1 core reserved, the 9-core exclusive #2 fits in 10.
    let e = present(state(vec![active(ticket(1, Side::Wsl, 1.0, 4, 60), H - 600, 8.0), ticket(2, Side::Wsl, 1.0, 9, 30)]), H);
    let p = schedule(&e, &m, &c, H);
    assert_eq!(p.events, vec![Event::Cleared(2)], "{}", p.reasons[&2].message);
    // Busy (load 3): still alone.
    m.wsl.as_mut().unwrap().load1 = 3.0;
    let p = schedule(&e, &m, &c, H);
    assert!(p.events.is_empty());
    assert_eq!(p.reasons[&2].reserve.as_deref(), Some("exclusive"));
}
