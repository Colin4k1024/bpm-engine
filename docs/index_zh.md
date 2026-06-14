---
layout: default
title: bpm-engine - 正确性优先的工作流执行引擎
description: 基于 Rust 的正确性优先、令牌驱动的工作流执行内核，专为确定性重放和崩溃安全的长时间运行流程而设计
lang: zh
---

<!-- Hero Section -->
<section class="hero">
  <div class="hero-content">
    <div class="hero-badge">
      <span class="hero-badge-dot"></span>
      <span>v0.2.0 · Rust · Apache-2.0</span>
    </div>
    <h1 class="hero-title">
      <span class="hero-title-gradient">bpm-engine</span>
    </h1>
    <p class="hero-subtitle">
      基于 Rust 的正确性优先、令牌驱动的工作流执行内核。<br>
      专为确定性重放和崩溃安全的长时间运行流程而设计。
    </p>
    <div class="hero-actions">
      <a href="#getting-started" class="btn btn-primary">
        <svg class="btn-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <polygon points="5 3 19 12 5 21 5 3"></polygon>
        </svg>
        快速开始
      </a>
      <a href="https://github.com/fanjia1024/bpm-engine" class="btn btn-secondary" target="_blank">
        <svg class="btn-icon" viewBox="0 0 16 16" fill="currentColor">
          <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/>
        </svg>
        GitHub
      </a>
    </div>
  </div>
</section>

<!-- 为什么选择 bpm-engine -->
<section class="section" id="why">
  <div class="section-header">
    <span class="section-tag">为什么选择 bpm-engine？</span>
    <h2 class="section-title">正确性优先于功能</h2>
    <p class="section-desc">
      大多数 BPM 引擎优化的是<strong>功能和建模体验</strong>。本引擎优化的是<strong>正确性</strong>。
      每个执行步骤都由数据库状态驱动，每个状态转换都会记录为历史。
    </p>
  </div>
</section>

<!-- 核心特性 -->
<section class="section" id="features">
  <div class="section-header">
    <span class="section-tag">核心保证</span>
    <h2 class="section-title">为可靠性而生</h2>
    <p class="section-desc">
      六项基本保证，使 bpm-engine 适用于关键任务工作流。
    </p>
  </div>
  
  <div class="features-grid">
    <div class="feature-card">
      <div class="feature-icon">⚡</div>
      <h3>精确一次完成</h3>
      <p>令牌精确到达最终状态一次。无重复执行，无状态丢失。引擎保证原子性完成。</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🔄</div>
      <h3>崩溃安全设计</h3>
      <p>引擎通过重放事件进行确定性恢复。终止进程、重启，然后从上次中断处继续。</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🔒</div>
      <h3>基于租约的外部任务</h3>
      <p>外部工作者按主题获取任务。租约保证同一时间只有一个所有者，并自动处理超时。</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">⏱️</div>
      <h3>持久化定时器</h3>
      <p>无内存定时器。所有定时器都被持久化，可在重启后存活。天然支持多实例扩展。</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">📜</div>
      <h3>完整审计历史</h3>
      <p>每个状态变更都会发出事件。历史记录仅追加且全局有序。非常适合合规和调试。</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🛡️</div>
      <h3>形式化不变量</h3>
      <p>连接节点等待所有分支完成。重试是单调递增的。核心行为通过不变量得到数学证明。</p>
    </div>
  </div>
</section>

<!-- 快速开始 -->
<section class="section" id="getting-started">
  <div class="section-header">
    <span class="section-tag">快速开始</span>
    <h2 class="section-title">5 分钟即可运行</h2>
    <p class="section-desc">
      开始使用 bpm-engine。默认的内存后端无需 Docker。
    </p>
  </div>
  
  <div class="code-section">
    <div class="code-tabs">
      <button class="tab-btn active" data-tab="install-zh">安装</button>
      <button class="tab-btn" data-tab="start-zh">启动引擎</button>
      <button class="tab-btn" data-tab="run-zh">运行流程</button>
      <button class="tab-btn" data-tab="worker-zh">外部工作者</button>
    </div>

    <div class="tab-content active" id="install-zh">
{% highlight bash %}
# 克隆仓库
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine

# 构建项目
cargo build
{% endhighlight %}
    </div>

    <div class="tab-content" id="start-zh">
{% highlight bash %}
# 启动 REST 服务器 (http://127.0.0.1:3000)
cargo run -p bpm-server-rest
{% endhighlight %}
<p class="code-note">
  内置流程定义：<code>minimal</code>（开始 → 结束），<code>payment-flow</code>（开始 → 外部任务 → 结束）
</p>
    </div>

    <div class="tab-content" id="run-zh">
{% highlight bash %}
# 运行最小流程（开始 → 结束）
cargo run --example simple_process

# 或使用 curl：
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"minimal"}'

# 检查实例状态
curl http://127.0.0.1:3000/api/v1/process-instances/:id

# 获取执行历史
curl http://127.0.0.1:3000/api/v1/process-instances/:id/history

# 获取聚合追踪
curl http://127.0.0.1:3000/api/v1/process-instances/:id/trace
{% endhighlight %}
    </div>

    <div class="tab-content" id="worker-zh">
{% highlight rust %}
use bpm_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, 
    TaskHandler, TaskResult, Worker, WorkerConfig
};

struct PaymentHandler;

#[async_trait::async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str { "payment" }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        // 业务逻辑
        let mut variables = std::collections::HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        TaskResult::Complete { variables }
    }
}

let worker = Worker::builder()
    .client(EngineClient::new("http://127.0.0.1:3000"))
    .handler(PaymentHandler)
    .config(WorkerConfig::new("worker-1")
        .poll_interval(std::time::Duration::from_secs(1)))
    .build();

worker.start().await;
{% endhighlight %}
    </div>
  </div>
</section>

<!-- 架构 -->
<section class="section" id="architecture">
  <div class="section-header">
    <span class="section-tag">架构设计</span>
    <h2 class="section-title">简洁而强大</h2>
    <p class="section-desc">
      清晰的分层架构，分离关注点，提供灵活性。
    </p>
  </div>

  <div class="arch-diagram">
    <pre>
┌─────────────────────────────────────────────────────────────────┐
│                        bpm-engine                               │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │   调度器     │  │    令牌      │  │      不变量          │  │
│  │  Scheduler   │  │   Executor   │  │     Invariants       │  │
│  └──────────────┘  └──────────────┘  └──────────────────────┘  │
│                            │                                    │
│                            ▼                                    │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                    持久化层                               │  │
│  │              (内存 / PostgreSQL)                          │  │
│  │    运行时表 │ 历史 │ 定时器 │ 外部任务                   │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                 │
│      外部工作者 (通过 REST API 获取 / 锁定 / 完成)             │
└─────────────────────────────────────────────────────────────────┘
    </pre>
  </div>

  <div class="arch-links">
    <a href="architecture_zh.html">📖 架构概览</a>
    <a href="execution-model.html">⚙️ 执行模型</a>
    <a href="invariants.html">🛡️ 不变量</a>
    <a href="recovery.html">💾 持久化与恢复</a>
  </div>
</section>

<!-- 工作空间 Crate -->
<section class="section" id="crates">
  <div class="section-header">
    <span class="section-tag">模块化设计</span>
    <h2 class="section-title">工作空间 Crate</h2>
    <p class="section-desc">
      组织良好的工作空间，每个 crate 都有清晰的职责。
    </p>
  </div>
  
  <div class="table-wrapper">
    <table>
      <thead>
        <tr>
          <th>Crate</th>
          <th>职责</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <td><code>bpm-core</code></td>
          <td>ProcessDefinition、NodeType、Token、EngineEvent、Saga。纯逻辑，无 I/O。</td>
        </tr>
        <tr>
          <td><code>bpm-storage</code></td>
          <td>异步持久化 trait（ProcessInstanceStore、TokenStore、ExternalTaskStore、TimerStore）</td>
        </tr>
        <tr>
          <td><code>bpm-runtime</code></td>
          <td>BpmEngine 事件循环、EngineContext、事件处理器、网关评估</td>
        </tr>
        <tr>
          <td><code>bpm-adapter-memory</code></td>
          <td>内存实现。默认用于开发/测试</td>
        </tr>
        <tr>
          <td><code>bpm-adapter-postgres</code></td>
          <td>PostgreSQL 实现。生产就绪的持久化</td>
        </tr>
        <tr>
          <td><code>bpm-bpmn</code></td>
          <td>BPMN 2.0 XML 解析器 → ProcessDefinition 编译器</td>
        </tr>
        <tr>
          <td><code>bpm-server-rest</code></td>
          <td>HTTP API 服务器（axum）。连接 EngineContext 与存储适配器</td>
        </tr>
        <tr>
          <td><code>bpm-worker-sdk</code></td>
          <td>EngineClient、Worker、TaskHandler。工作者是无状态且可水平扩展的</td>
        </tr>
      </tbody>
    </table>
  </div>
</section>

<!-- REST API -->
<section class="section" id="api">
  <div class="section-header">
    <span class="section-tag">API 参考</span>
    <h2 class="section-title">REST API</h2>
    <p class="section-desc">
      清洁的 RESTful API，用于所有引擎操作。基础路径：<code>/api/v1</code>
    </p>
  </div>

  <div class="table-wrapper">
    <table>
      <thead>
        <tr>
          <th>方法</th>
          <th>路径</th>
          <th>描述</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/process-instances</code></td>
          <td>启动实例。Body: <code>{"process_def_id", "variables"?: {}}</code></td>
        </tr>
        <tr>
          <td><span class="method get">GET</span></td>
          <td><code>/process-instances/:id</code></td>
          <td>获取实例状态和当前节点</td>
        </tr>
        <tr>
          <td><span class="method get">GET</span></td>
          <td><code>/process-instances/:id/history</code></td>
          <td>获取执行历史（带序列号和类别的事件）</td>
        </tr>
        <tr>
          <td><span class="method get">GET</span></td>
          <td><code>/process-instances/:id/trace</code></td>
          <td>获取聚合追踪（令牌时间线和外部任务历史）</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/process-definitions/deploy</code></td>
          <td>从 BPMN 2.0 XML 部署流程（Body: 原始 XML）</td>
        </tr>
        <tr>
          <td><span class="method get">GET</span></td>
          <td><code>/tasks?type=user|external</code></td>
          <td>列出等待中的任务</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/tasks/:task_id/complete</code></td>
          <td>完成用户任务</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/external-tasks/fetch-and-lock</code></td>
          <td>工作者：获取并锁定任务</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/external-tasks/:task_id/complete</code></td>
          <td>工作者：完成任务</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/external-tasks/:task_id/fail</code></td>
          <td>工作者：任务失败</td>
        </tr>
      </tbody>
    </table>
  </div>

  <p class="code-note" style="text-align: center; margin-top: 24px;">
    可选头部：<code>x-tenant-id</code> 用于租户隔离
  </p>
</section>

<!-- 文档链接 -->
<section class="section" id="docs">
  <div class="section-header">
    <span class="section-tag">文档</span>
    <h2 class="section-title">了解更多</h2>
    <p class="section-desc">
      全面的文档帮助您理解和有效使用 bpm-engine。
    </p>
  </div>
  
  <div class="docs-grid">
    <div class="doc-card">
      <div class="doc-card-icon">📖</div>
      <h3>核心概念</h3>
      <ul>
        <li><a href="architecture_zh.html">架构概览</a></li>
        <li><a href="execution-model.html">执行模型</a></li>
        <li><a href="invariants.html">形式化不变量</a></li>
        <li><a href="why-correctness.html">为什么正确性重要</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <div class="doc-card-icon">🔧</div>
      <h3>开发指南</h3>
      <ul>
        <li><a href="quick-start_zh.html">快速开始指南</a></li>
        <li><a href="sdk-rust.html">Rust Worker SDK</a></li>
        <li><a href="bpmn_zh.html">BPMN 2.0 支持</a></li>
        <li><a href="api-reference_zh.html">API 规范</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <div class="doc-card-icon">🛡️</div>
      <h3>可靠性</h3>
      <ul>
        <li><a href="recovery.html">持久化与恢复</a></li>
        <li><a href="accident-scenarios.html">事故场景</a></li>
        <li><a href="database-schema.html">数据库模式</a></li>
        <li><a href="saga.html">Saga 与补偿</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <div class="doc-card-icon">❓</div>
      <h3>参考资料</h3>
      <ul>
        <li><a href="faq.html">常见问题</a></li>
        <li><a href="cheat-sheet.html">速查表</a></li>
        <li><a href="roadmap.html">路线图</a></li>
        <li><a href="https://github.com/fanjia1024/bpm-engine/blob/master/CONTRIBUTING.md">贡献指南</a></li>
      </ul>
    </div>
  </div>
</section>

<!-- 项目状态 -->
<section class="section">
  <div class="status-section">
    <div class="section-header">
      <span class="section-tag">项目状态</span>
      <h2 class="section-title">当前状态</h2>
    </div>

    <div class="status-grid">
      <div class="status-item">
        <span class="status-badge status-stable">稳定</span>
        <p>核心执行语义</p>
      </div>
      <div class="status-item">
        <span class="status-badge status-evolving">演进中</span>
        <p>API 可能会变化</p>
      </div>
      <div class="status-item">
        <span class="status-badge status-research">研究</span>
        <p>生产用例</p>
      </div>
    </div>

    <p class="status-note">
      该引擎适用于：<strong>研究</strong>、<strong>原型开发</strong>、<strong>内部系统</strong>、<strong>正确性优先的实验</strong>
    </p>
  </div>
</section>
