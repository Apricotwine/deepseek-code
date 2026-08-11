# 论文与投稿路线图

> 工程实现 + 测评 → 论文 → arXiv → ICLR 2027 海报

## 目录内容

| 文件 | 说明 |
|---|---|
| `paper.tex` | LaTeX 正式稿（投稿用，当前为 article 版式，ICLR 投稿时换成官方 style） |
| `paper.md` | 同内容的可读 Markdown |
| `preview.html` | 深蓝海洋主题的 HTML 预览（直接浏览器打开） |
| `figures/` | 架构图 + 结果图（matplotlib 生成，PDF+PNG） |
| `scripts/` | 图表生成与 HTML 预览渲染脚本 |
| `benchmarks/temporal_harness_result_large_flash.json` | Flash 大规模测评（72 T1 / 72 T3 / 9 T2） |
| `benchmarks/temporal_harness_result_large_pro.json` | Pro 大规模测评（同上） |
| `benchmarks/cache_probe_{short,long}.json` | 前缀缓存实测（300 / 1200 字符消息） |
| `benchmarks/temporal_harness_result_v1..v3.json` | 早期小样本轮次（保留可复现性） |

## 关键时间线（ICLR 2027）

| 事项 | 日期 |
|---|---|
| arXiv 预印本 | 随时可挂（ICLR 官方允许投稿期间挂 arXiv） |
| ICLR 2027 摘要截止 | **2026-09-18**（AOE，必须先交摘要，之后不能再加作者） |
| ICLR 2027 全文截止 | **2026-09-25**（AOE） |
| 审稿放出 / 决定 | 2026-11-05 / 2026-12-16 |

现在（8/6）到摘要截止还有 **6 周**，节奏完全够。

## 提交前 Checklist

### 内容强化（优先）
- [x] 对抗性对照组（标签翻转）：Flash 26.4% / Pro 30.6%（低于随机 50%）→ 核心诊断
- [x] 大规模程序化用例：72 T1 / 72 T3 声明 / 9 T2 任务，双种子
- [x] 模型横评：Flash + Pro（跨厂端点仍未做）
- [x] 缓存实测：TAL 下缓存命中份额 99.5%（无 TAL 98.0%），每消息 ~22 token
- [x] 解析器加固：JSON 围栏容忍 + 长度守卫，消除条件相关解析偏置
- [x] 决策链提示实验：标签翻转下 Flash/Pro 均从 26–31% 回升到 **98.6%**（正确标签下 100%，顺带消除边界误差）→ 论文核心机制发现
- [ ] 跨厂端点（OpenAI 兼容协议）验证 harness 协议无关性
- [ ] 真实会话陈旧引用率田野统计（TAL off vs on）
- [ ] 多轮工具循环中决策程序鲁棒性 + 无标签（纯时间戳）条件

### 投稿操作
- [ ] arXiv：注册账号 + 找一位有 arXiv 背书资格的导师/同事背书，上传匿名前版本
- [ ] ICLR：OpenReview 注册，**9/18 前**交真实摘要（作者名单锁定）
- [ ] ICLR 版本必须完全匿名：正文/补充材料不得出现姓名、机构、可识别的仓库链接
- [ ] 换 ICLR 官方 LaTeX 模板（`iclr2027.sty`），正文 ≤ 9 页，参考文献不计页
- [ ] 按 ICLR 政策提交 LLM 使用声明（论文附录已起草）

### 合规要点（ICLR 2026 政策，2027 预计延续）
- arXiv 预印本**不违反**双重投稿政策，审查期间挂 arXiv 也允许
- 作者自己相关 arXiv 论文需第三人称引用（当前没有此问题）
- LLM 深度参与写作需在附录披露，不披露可能直接 desk reject

## 论文核心叙事

1. **问题**：1M 上下文让"陈旧数据"成为一等公民；模型无时钟，昨天的输出 = 今天的现状。
2. **方案**：请求时注入时间感知层（L0 时钟 / L1 新鲜度 / L3-4 跨会话跨度），存储保持原始、缓存前缀稳定。
3. **证据**：T1 Flash 62.5%→91.7%、Pro 44.4%→100%（学会信任新鲜数据）；T3 增益温和（+2.8 点，集中在顺序违规）；T2 两模型方向相反（Flash 高估 3.7× / Pro 低估 0.56×）。
4. **机制发现**：标签翻转后 T1 掉到 26–31%（模型"跟标签"），但决策链提示（先算 age、再比对 horizon、标签可能错）把两个模型都拉回 98.6%——注解提供信息、决策程序决定模型是否使用它，"stamps + decision procedure" 才是完整的时间接地；同时 T3 早期 +16.7 点是解析偏置假象，已如实修正。

## 生成命令

```bash
# 图表
MPLCONFIGDIR=/tmp/mplcfg python3 paper/scripts/make_figures.py
# HTML 预览（paper.md → preview.html）
python3 paper/scripts/md2html.py
# 完整测评（含对照组，需 DeepSeek key）
python3 benchmarks/temporal_harness_bench_large.py --t1 36 --t3 12 --t2 9 --seeds 7 11 --workers 4 --out result.json
# 缓存实测
DS_MODEL=deepseek-v4-flash python3 benchmarks/cache_probe.py --turns 60 --msg-len 1200 --out cache_probe_long.json
# LaTeX 编译（本机无 TeX；装 tectonic 或 TinyTeX 后可编译）
pdflatex paper/paper.tex
```
