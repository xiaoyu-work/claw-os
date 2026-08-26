// Claw OS website — minimal, framework-free interactions.

(function () {
  "use strict";

  // ---------- Sticky nav shadow ----------
  const nav = document.getElementById("nav");
  if (nav) {
    const onScroll = () => nav.classList.toggle("is-scrolled", window.scrollY > 8);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
  }

  // ---------- Mobile nav ----------
  const burger = document.getElementById("navBurger");
  if (burger && nav) {
    burger.addEventListener("click", () => {
      const open = nav.classList.toggle("is-mobile-open");
      burger.setAttribute("aria-expanded", open ? "true" : "false");
    });
    nav.querySelectorAll(".nav-links a").forEach((a) =>
      a.addEventListener("click", () => {
        nav.classList.remove("is-mobile-open");
        burger.setAttribute("aria-expanded", "false");
      })
    );
  }

  // ---------- Copy-to-clipboard ----------
  const flashCopied = (btn) => {
    const prev = btn.textContent;
    btn.textContent = "copied";
    btn.classList.add("is-copied");
    setTimeout(() => {
      btn.textContent = prev;
      btn.classList.remove("is-copied");
    }, 1400);
  };
  document.querySelectorAll(".copy-btn").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const sel = btn.getAttribute("data-copy");
      const target = sel ? document.querySelector(sel) : null;
      if (!target) return;
      const text = target.innerText.replace(/\u00A0/g, " ").trim();
      try {
        await navigator.clipboard.writeText(text);
        flashCopied(btn);
      } catch (_) {
        const r = document.createRange();
        r.selectNodeContents(target);
        const sel2 = window.getSelection();
        sel2.removeAllRanges();
        sel2.addRange(r);
        try { document.execCommand("copy"); flashCopied(btn); } catch (e) { /* noop */ }
        sel2.removeAllRanges();
      }
    });
  });

  // ---------- Guided Claw OS experience ----------
  const demo = document.getElementById("clawOsDemo");
  if (demo) {
    const agentImage = "../assets/brand/clawos-symbol.png";
    const scenarios = {
      health: {
        icon: "◎",
        appTitle: "System Monitor",
        appSubtitle: "Live system state",
        model: "qwen3 · local",
        prompt: "Why is my network slow right now?",
        intro: "I’ll correlate network activity, running apps, and recent service logs before I explain the cause.",
        plan: [
          "Measure current sockets, routes, DNS latency, and throughput.",
          "Find the apps and background jobs using the most bandwidth.",
          "Correlate NetworkManager logs with the activity timeline.",
        ],
        scopes: [
          ["sys.net.read", "interfaces, routes, sockets"],
          ["app.activity.read", "network usage by installed apps"],
          ["log.read", "NetworkManager · last 15 minutes"],
        ],
        tools: [
          ["sys.net.snapshot", "42 sockets · 2 active routes"],
          ["app.activity.list", "Photo Sync is the top uploader"],
          ["log.query", "No link or DNS failures found"],
        ],
        before: {
          kicker: "SYSTEM HEALTH",
          title: "Network activity",
          description: "Live traffic, latency, and the apps currently using your connection.",
          metrics: [["Upload", "47.8 Mbps", "93% of uplink", "alert"], ["DNS", "42 ms", "healthy", "good"], ["Packet loss", "0.0%", "last 5 minutes", "good"]],
          evidence: [["Photo Sync", "Uploading 1,284 photos", "47.1 Mbps", "alert"], ["Web Browser", "12 active connections", "620 Kbps"], ["NetworkManager", "Link stable since 09:14", "healthy", "good"]],
        },
        after: {
          kicker: "ROOT CAUSE FOUND",
          title: "Photo Sync is saturating upload",
          description: "The connection is healthy. A background photo upload is consuming almost all upstream bandwidth.",
          metrics: [["Photo Sync", "47.1 Mbps", "98% of upload", "alert"], ["Link", "1 Gbps", "connected", "good"], ["DNS", "42 ms", "no failures", "good"]],
          evidence: [["App activity", "Photo Sync started at 10:21", "matched", "alert"], ["System network", "Other traffic below 1 Mbps", "normal", "good"], ["Service logs", "No driver, link, or DNS errors", "clear", "good"]],
        },
        resultTitle: "Your network is healthy.",
        result: "Photo Sync is uploading 1,284 files at 47 Mbps and saturating the uplink. Pausing that job will restore interactive traffic immediately.",
        audit: "task net-8f21 · 3 reads · no system changes",
      },
      crash: {
        icon: "!",
        appTitle: "App Inspector",
        appSubtitle: "Photos · crash report",
        model: "qwen3 · local",
        prompt: "Why did Photos crash, what could it access, and what happened before it failed?",
        intro: "I’ll inspect the app manifest, its scoped history, and the crash evidence without opening unrelated app data.",
        plan: [
          "Read the Photos capability manifest and current grants.",
          "Inspect its recent app activity and resource history.",
          "Correlate the crash dump with journal and audit events.",
        ],
        scopes: [
          ["app.inspect", "Photos manifest and grants"],
          ["app.history.read", "Photos · last 30 minutes"],
          ["log.read", "Photos crash and journal evidence"],
        ],
        tools: [
          ["app.schema", "pictures.read · gpu.use · no network"],
          ["app.history", "Opened panorama-final.tiff"],
          ["log.crash", "OOM kill at 4.1 GiB resident memory"],
        ],
        before: {
          kicker: "APP CRASH",
          title: "Photos stopped unexpectedly",
          description: "Inspect permissions, activity, logs, and resource evidence in one place.",
          metrics: [["Exit", "SIGKILL", "10:28:14", "alert"], ["Peak memory", "4.1 GiB", "limit 4 GiB", "alert"], ["Network", "Denied", "not requested", "good"]],
          evidence: [["Permissions", "Pictures read + GPU acceleration", "2 scopes"], ["Activity", "Opened panorama-final.tiff", "1.8 GiB"], ["Crash journal", "Process killed by cgroup memory limit", "new", "alert"]],
        },
        after: {
          kicker: "CRASH EXPLAINED",
          title: "Memory limit exceeded",
          description: "Photos expanded a large TIFF past its 4 GiB cgroup limit. It never had network access.",
          metrics: [["Image decode", "3.6 GiB", "largest allocation", "alert"], ["App limit", "4.0 GiB", "enforced"], ["Data access", "Pictures", "network denied", "good"]],
          evidence: [["10:27:51", "Opened panorama-final.tiff", "history"], ["10:28:12", "Decoder reached 4.1 GiB", "memory", "alert"], ["10:28:14", "cgroup terminated process", "crash", "alert"]],
        },
        resultTitle: "Photos exceeded its memory boundary.",
        result: "The 1.8 GiB TIFF expanded to 4.1 GiB during decoding. The OS stopped Photos at its 4 GiB limit. It could read Pictures and use the GPU; network access was never granted.",
        audit: "task crash-3a17 · app-scoped evidence only",
      },
      workflow: {
        icon: "⇄",
        appTitle: "Workflow",
        appSubtitle: "Files → AI → Mail → Calendar",
        model: "claude · cloud",
        prompt: "Summarize the Q3 plan, draft an email to the team, and schedule a review Friday.",
        intro: "I’ll compose typed operations from four apps and stop before sending or creating anything until you approve.",
        plan: [
          "Read Q3-plan.md from Files and summarize it with the system AI gate.",
          "Create a Mail draft addressed to the project team.",
          "Create a Calendar review event for Friday at 2 PM.",
        ],
        scopes: [
          ["app.call", "Files · document.read · Q3-plan.md"],
          ["ai.chat", "summarize · project content"],
          ["app.call", "Mail draft + Calendar event"],
        ],
        tools: [
          ["files.document.read", "Q3-plan.md · 18 pages"],
          ["ai.chat", "Local policy chose configured cloud model"],
          ["mail.draft.create", "Draft saved · not sent"],
          ["calendar.event.create", "Friday 2:00–2:45 PM"],
        ],
        before: {
          kicker: "CROSS-APP WORKFLOW",
          title: "Four apps, one request",
          description: "The agent discovers app APIs and composes them without scraping their UI.",
          metrics: [["Apps", "4", "typed operations"], ["Steps", "4", "approval gated"], ["External sends", "0", "draft only", "good"]],
          evidence: [["Files", "Q3-plan.md selected", "ready"], ["System AI", "Summarize with approved provider", "ready"], ["Mail + Calendar", "Waiting for content", "queued"]],
        },
        after: {
          kicker: "WORKFLOW COMPLETE",
          title: "Review package is ready",
          description: "The source was summarized, an email draft was created, and the review was placed on the calendar.",
          metrics: [["Summary", "6 points", "from 18 pages", "good"], ["Mail", "Draft ready", "not sent", "good"], ["Calendar", "Fri 2:00 PM", "45 minutes", "good"]],
          evidence: [["Files", "Q3-plan.md read with document.read", "complete", "good"], ["Mail", "Team update saved to Drafts", "complete", "good"], ["Calendar", "Q3 review created with 8 attendees", "complete", "good"]],
        },
        resultTitle: "The cross-app workflow is complete.",
        result: "I summarized the six Q3 priorities, created a team email draft, and scheduled a 45-minute review for Friday at 2 PM. Nothing was sent without your approval.",
        audit: "task flow-92c4 · 4 app calls · 1 model call",
      },
      models: {
        icon: "AI",
        appTitle: "AI Runtime",
        appSubtitle: "Shared models for every app",
        model: "qwen3 · local",
        prompt: "Use the system model to summarize this note for the Notes app.",
        intro: "Notes can use the OS model layer without bundling a model, provider SDK, credential store, or safety pipeline.",
        plan: [
          "Validate the Notes app AI declaration and user consent.",
          "Route the request to the available local model.",
          "Record usage and return only the generated text to Notes.",
        ],
        scopes: [
          ["ai.chat", "Notes · summarize user-authored text"],
          ["model.use", "qwen3-8b · local NPU"],
          ["audit.write", "usage, latency, and model identity"],
        ],
        tools: [
          ["ai.policy.check", "Notes consent and budget valid"],
          ["model.route", "qwen3-8b selected · local"],
          ["ai.chat", "214 tokens · 640 ms"],
        ],
        before: {
          kicker: "SYSTEM AI GATE",
          title: "One model layer for every app",
          description: "Provider choice, credentials, consent, budgets, and logs stay owned by Claw OS.",
          metrics: [["Model", "qwen3-8b", "local NPU", "good"], ["App", "Notes", "consent granted"], ["Budget", "3,588 left", "monthly units", "good"]],
          evidence: [["Notes", "Requests ai.chat only", "allowed", "good"], ["Credential store", "No provider key exposed to app", "isolated", "good"], ["Fallback", "Cloud disabled for this request", "local", "good"]],
        },
        after: {
          kicker: "AI REQUEST COMPLETE",
          title: "Notes received the summary",
          description: "The app received text and usage metadata; model and credential complexity remained inside the OS.",
          metrics: [["Latency", "640 ms", "local NPU", "good"], ["Usage", "214 tokens", "recorded"], ["App secrets", "0", "none exposed", "good"]],
          evidence: [["Policy", "Consent + origin + budget checked", "passed", "good"], ["Runtime", "qwen3-8b on NPU", "local", "good"], ["Audit", "Model identity and usage recorded", "complete", "good"]],
        },
        resultTitle: "Notes used AI without owning an AI stack.",
        result: "The local qwen3 model produced the summary in 640 ms. Notes received the result and usage metadata but never handled model files, provider credentials, or fallback logic.",
        audit: "task ai-5d60 · local model · 214 tokens",
      },
      memory: {
        icon: "M",
        appTitle: "Memory",
        appSubtitle: "Cross-app recall",
        model: "qwen3 · local",
        prompt: "What did I decide about the launch plan last week?",
        intro: "I’ll search your approved work memory across conversations and app sessions, then cite where each conclusion came from.",
        plan: [
          "Search semantic memory for launch-plan decisions.",
          "Open the matching Files, Mail, and Calendar session records.",
          "Synthesize the decision with citations and memory controls.",
        ],
        scopes: [
          ["memory.recall", "work memories · launch plan"],
          ["session.read", "Files, Mail, Calendar matches"],
          ["ai.chat", "synthesize cited records"],
        ],
        tools: [
          ["memory.search", "7 matches across 3 apps"],
          ["session.read", "3 high-confidence records opened"],
          ["ai.chat", "Decision synthesized with citations"],
        ],
        before: {
          kicker: "PERSISTENT MEMORY",
          title: "Context follows you across apps",
          description: "Searchable memories remain inspectable, attributable, and removable.",
          metrics: [["Matches", "7", "last week"], ["Apps", "3", "Files · Mail · Calendar"], ["Confidence", "94%", "cited records", "good"]],
          evidence: [["Files", "launch-plan.md edited Tuesday", "memory"], ["Mail", "Beta date confirmed with team", "session"], ["Calendar", "Launch review moved one week", "event"]],
        },
        after: {
          kicker: "DECISION RECALLED",
          title: "Public beta: September 18",
          description: "The date and rollout sequence agree across the plan, team email, and calendar.",
          metrics: [["Decision", "Sep 18", "public beta", "good"], ["Rollout", "3 stages", "internal → beta → public"], ["Sources", "3", "fully cited", "good"]],
          evidence: [["launch-plan.md", "Three-stage rollout approved", "Tue 14:08", "good"], ["Team email", "September 18 confirmed", "Wed 09:31", "good"], ["Calendar", "Go/no-go review on September 15", "Thu 16:20", "good"]],
        },
        resultTitle: "You chose a three-stage launch.",
        result: "Internal dogfood starts September 4, the private beta starts September 11, and the public beta is September 18 after the September 15 go/no-go review.",
        audit: "task mem-b884 · 3 memories cited · forget controls available",
      },
      access: {
        icon: "✓",
        appTitle: "App Access",
        appSubtitle: "Permissions and recent activity",
        model: "qwen3 · local",
        prompt: "Show what my apps can access and which apps used AI today.",
        intro: "I’ll read app manifests, active grants, and today’s audit history, then highlight anything unusual.",
        plan: [
          "List installed app capability declarations and current grants.",
          "Query today’s AI and privileged-operation audit records.",
          "Explain outliers and provide revocation paths.",
        ],
        scopes: [
          ["app.inspect", "installed manifests and grants"],
          ["audit.read", "today · app and AI activity"],
          ["caps.list", "active approvals and expiry"],
        ],
        tools: [
          ["app.list", "18 installed apps"],
          ["caps.grants", "31 grants · 2 temporary"],
          ["audit.query", "3 AI calls · 1 denied network call"],
        ],
        before: {
          kicker: "APP ACCESS",
          title: "Permissions are visible system state",
          description: "See what every app may access and what it actually did.",
          metrics: [["Apps", "18", "installed"], ["Active grants", "31", "2 temporary"], ["AI calls today", "3", "2 apps"]],
          evidence: [["Photos", "Pictures read · GPU · no network", "2 grants", "good"], ["Mail", "Network · contacts · AI summarize", "5 grants"], ["Notes", "Documents read/write · local AI", "4 grants"]],
        },
        after: {
          kicker: "ACCESS REVIEW",
          title: "No unexpected privileged access",
          description: "Two apps used AI today. A Weather network request was denied because its temporary grant expired.",
          metrics: [["AI apps", "Mail + Notes", "3 calls", "good"], ["Denied", "1", "Weather network", "alert"], ["Expiring", "2 grants", "within 24 hours"]],
          evidence: [["Mail", "2 summaries via configured cloud model", "today", "good"], ["Notes", "1 summary via local qwen3", "today", "good"], ["Weather", "net.dial denied after grant expiry", "review", "alert"]],
        },
        resultTitle: "Your app access matches policy.",
        result: "Mail and Notes used the system AI gate three times today. Photos still has no network access. Weather’s expired temporary network grant was correctly denied and can be renewed or removed.",
        audit: "task access-0e19 · read-only review · no grants changed",
      },
    };

    const scenarioButtons = [...document.querySelectorAll("[data-sim-scenario]")];
    const dockButtons = [...document.querySelectorAll("[data-sim-dock]")];
    const guideItems = [...document.querySelectorAll("[data-sim-guide]")];
    const refs = {
      workspace: document.getElementById("simWorkspace"),
      chat: document.getElementById("simChat"),
      promptForm: document.getElementById("simPromptForm"),
      promptInput: document.getElementById("simPromptInput"),
      promptSend: document.getElementById("simPromptSend"),
      next: document.getElementById("simNextButton"),
      stepLabel: document.getElementById("simStepLabel"),
      stepTitle: document.getElementById("simStepTitle"),
      stepHelp: document.getElementById("simStepHelp"),
      appMark: document.getElementById("simAppMark"),
      appTitle: document.getElementById("simAppTitle"),
      appSubtitle: document.getElementById("simAppSubtitle"),
      model: document.getElementById("simModelPill"),
      fullscreen: document.getElementById("simFullscreenButton"),
      restart: document.getElementById("simRestartButton"),
    };
    const steps = [
      ["STEP 1 OF 4", "Choose a task", "Select a task above, then send its suggested prompt.", "Send suggested prompt"],
      ["STEP 2 OF 4", "Review the agent’s plan", "Claw Agent explains the operations it wants to perform before requesting access.", "Review requested access"],
      ["STEP 3 OF 4", "Approve exact access", "Nothing runs until you approve these capability scopes.", "Allow once"],
      ["STEP 4 OF 4", "Inspect structured evidence", "Approved tools returned system and app evidence. Continue for the explanation.", "Show agent answer"],
      ["COMPLETE", "Result recorded in audit history", "Review the answer and evidence, or replay this task.", "Replay this task"],
    ];
    let activeId = "health";
    let step = 0;
    let submittedPrompt = scenarios[activeId].prompt;

    const create = (tag, className, text) => {
      const node = document.createElement(tag);
      if (className) node.className = className;
      if (text !== undefined) node.textContent = text;
      return node;
    };

    const agentAvatar = (className = "sim-agent-mini") => {
      const avatar = create("span", className);
      const image = document.createElement("img");
      image.src = agentImage;
      image.alt = "";
      avatar.append(image);
      return avatar;
    };

    const renderWorkspace = () => {
      const data = scenarios[activeId];
      const state = step >= 3 ? data.after : data.before;
      refs.workspace.replaceChildren();

      const head = create("div", "sim-workspace-head");
      const copy = create("div");
      copy.append(
        create("div", "sim-workspace-kicker mono", state.kicker),
        create("h3", "", state.title),
        create("p", "", state.description)
      );
      const live = create("span", "sim-live-pill mono");
      live.append(create("i"), document.createTextNode(step >= 3 ? " correlated" : " live"));
      head.append(copy, live);

      const metrics = create("div", "sim-metrics");
      state.metrics.forEach(([label, value, detail, status]) => {
        const metric = create("div", `sim-metric${status ? ` is-${status}` : ""}`);
        metric.append(create("small", "", label), create("strong", "", value), create("span", "", detail));
        metrics.append(metric);
      });

      const evidence = create("div", "sim-evidence");
      const evidenceHead = create("div", "sim-evidence-head mono");
      evidenceHead.append(create("span", "", step >= 3 ? "Correlated evidence" : "Live evidence"), create("span", "", `${state.evidence.length} sources`));
      evidence.append(evidenceHead);
      state.evidence.forEach(([source, detail, tag, status]) => {
        const row = create("div", `sim-evidence-row${status ? ` is-${status}` : ""}`);
        const body = create("div");
        body.append(create("strong", "", source), create("small", "", detail));
        row.append(create("i", "sim-evidence-dot"), body, create("span", "sim-evidence-tag mono", tag));
        evidence.append(row);
      });

      refs.workspace.append(head, metrics, evidence);
      refs.appMark.textContent = data.icon;
      refs.appTitle.textContent = data.appTitle;
      refs.appSubtitle.textContent = data.appSubtitle;
      refs.model.textContent = data.model;
    };

    const addAssistantMessage = (text) => {
      const message = create("div", "sim-message");
      message.append(agentAvatar(), create("div", "sim-message-bubble", text));
      refs.chat.append(message);
    };

    const addUserMessage = (text) => {
      const message = create("div", "sim-message sim-message-user");
      message.append(create("div", "sim-message-bubble", text));
      refs.chat.append(message);
    };

    const renderChat = () => {
      const data = scenarios[activeId];
      refs.chat.replaceChildren();
      if (step === 0) {
        const empty = create("div", "sim-chat-empty");
        const body = create("div");
        body.append(
          agentAvatar("sim-agent-avatar"),
          create("h3", "", "What should I do?"),
          create("p", "", "Send the suggested prompt to let the system agent inspect Linux and your apps through scoped tools.")
        );
        empty.append(body);
        refs.chat.append(empty);
        return;
      }

      addUserMessage(submittedPrompt);
      addAssistantMessage(data.intro);

      const plan = create("div", "sim-plan");
      const planHead = create("div", "sim-card-head");
      planHead.append(create("strong", "", "Plan"), create("span", "mono", `${data.plan.length} steps`));
      const list = create("ol");
      data.plan.forEach((item) => list.append(create("li", "", item)));
      plan.append(planHead, list);
      refs.chat.append(plan);

      if (step >= 2) {
        const approval = create("div", "sim-approval");
        const approvalHead = create("div", "sim-card-head");
        approvalHead.append(create("strong", "", "Approval required"), create("span", "mono", "allow once"));
        approval.append(approvalHead);
        data.scopes.forEach(([scope, target]) => {
          const row = create("div", "sim-scope");
          row.append(create("code", "mono", scope), create("span", "", target));
          approval.append(row);
        });
        refs.chat.append(approval);
      }

      if (step >= 3) {
        const trace = create("div", "sim-tool-trace");
        const traceHead = create("div", "sim-card-head");
        traceHead.append(create("strong", "", "Tool execution"), create("span", "mono", "structured output"));
        trace.append(traceHead);
        data.tools.forEach(([tool, detail]) => {
          const row = create("div", "sim-tool");
          const body = create("div");
          body.append(create("strong", "mono", tool), create("small", "", detail));
          row.append(create("span", "sim-tool-check", "✓"), body, create("span", "mono", "done"));
          trace.append(row);
        });
        refs.chat.append(trace);
      }

      if (step >= 4) {
        addAssistantMessage(data.result);
        const result = create("div", "sim-result-card");
        result.append(create("strong", "", data.resultTitle), create("p", "", data.result));
        const audit = create("div", "sim-audit-line mono");
        audit.append(create("i"), create("span", "", data.audit));
        result.append(audit);
        refs.chat.append(result);
      }

      requestAnimationFrame(() => {
        refs.chat.scrollTop = refs.chat.scrollHeight;
      });
    };

    const render = () => {
      const data = scenarios[activeId];
      demo.dataset.simStep = String(step);
      scenarioButtons.forEach((button) => {
        const active = button.dataset.simScenario === activeId;
        button.classList.toggle("is-active", active);
        button.setAttribute("aria-selected", active ? "true" : "false");
      });
      dockButtons.forEach((button) => {
        button.classList.toggle("is-active", button.dataset.simDock === activeId);
      });

      const guideIndex = step === 0 ? 0 : step === 1 ? 1 : step === 2 ? 2 : 3;
      guideItems.forEach((item, index) => {
        item.classList.toggle("is-active", index === guideIndex);
        item.classList.toggle("is-complete", step === 4 || index < guideIndex);
      });

      const [label, title, help, action] = steps[step];
      refs.stepLabel.textContent = label;
      refs.stepTitle.textContent = title;
      refs.stepHelp.textContent = help;
      refs.next.firstChild.textContent = `${action} `;
      refs.promptInput.disabled = step !== 0;
      refs.promptSend.disabled = step !== 0;
      if (step === 0) refs.promptInput.value = data.prompt;
      renderWorkspace();
      renderChat();
    };

    const selectScenario = (id, focusPrompt = false) => {
      if (!scenarios[id]) return;
      activeId = id;
      step = 0;
      submittedPrompt = scenarios[id].prompt;
      render();
      if (focusPrompt) refs.promptInput.focus();
    };

    const advance = () => {
      if (step === 0) {
        submittedPrompt = refs.promptInput.value.trim() || scenarios[activeId].prompt;
        step = 1;
      } else if (step < 4) {
        step += 1;
      } else {
        step = 0;
        submittedPrompt = scenarios[activeId].prompt;
      }
      render();
    };

    scenarioButtons.forEach((button, index) => {
      button.addEventListener("click", () => selectScenario(button.dataset.simScenario, true));
      button.addEventListener("keydown", (event) => {
        if (!["ArrowLeft", "ArrowRight"].includes(event.key)) return;
        event.preventDefault();
        const offset = event.key === "ArrowRight" ? 1 : -1;
        const nextIndex = (index + offset + scenarioButtons.length) % scenarioButtons.length;
        scenarioButtons[nextIndex].focus();
        selectScenario(scenarioButtons[nextIndex].dataset.simScenario);
      });
    });
    dockButtons.forEach((button) => {
      button.addEventListener("click", () => selectScenario(button.dataset.simDock, true));
    });
    refs.promptForm.addEventListener("submit", (event) => {
      event.preventDefault();
      if (step === 0) advance();
    });
    refs.next.addEventListener("click", advance);
    refs.restart.addEventListener("click", () => selectScenario(activeId));
    refs.fullscreen.addEventListener("click", async () => {
      try {
        if (document.fullscreenElement === demo) {
          await document.exitFullscreen();
        } else {
          await demo.requestFullscreen();
        }
      } catch (error) {
        refs.stepHelp.textContent = `Full screen could not open: ${error.message}`;
      }
    });
    document.addEventListener("fullscreenchange", () => {
      const active = document.fullscreenElement === demo;
      refs.fullscreen.setAttribute("aria-label", active ? "Exit full screen" : "Enter full screen");
      const label = refs.fullscreen.querySelector(".sim-fullscreen-label");
      if (label) label.textContent = active ? "Exit full screen" : "Full screen";
    });

    render();
  }
})();
