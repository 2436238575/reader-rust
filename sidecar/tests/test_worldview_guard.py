"""worldview 防章节复述：取自 aiBookGeneration.test.ts 的真实对抗样例。"""

from agent_sidecar.worldview_guard import is_chapter_summary_worldview

NARRATIVE_RECAP = (
    "卢米安坐在屋顶沉思，他一直渴望获得超凡力量但奥萝尔拒绝教他，称这条路危险痛苦。"
    "回到房间后看到奥萝尔在用香槟金色钢笔给笔友写信，奥萝尔解释笔友是通过报纸专栏等认识的书信朋友，"
    "其中有厉害的人，电池灯就是笔友送的。卢米安躺在床上担心奥萝尔的秘密带来危险。"
    "随后卢米安反复做灰色雾气的梦，无论往哪走都会回到自己的卧室，频率越来越高几乎每天都会做。"
    "清晨卢米安告诉奥萝尔又做那个梦了，奥萝尔说之前的方案没用，考虑给他找一个真正的催眠师。"
    "卢米安想成为巫师解开梦境秘密，奥萝尔拒绝并说这个世界变得越来越危险，催促他准备考试。"
)


def test_title_mentioning_chapter_is_summary():
    assert (
        is_chapter_summary_worldview("本章（第11章）执法队搜查", "执法队搜查张家老屋一无所获。", "基础设定")
        is True
    )


def test_narrative_recap_content_is_summary():
    assert is_chapter_summary_worldview("梦境与巫师线索", NARRATIVE_RECAP, "基础设定") is True


def test_setting_content_is_kept():
    assert (
        is_chapter_summary_worldview(
            "超凡领域", "存在普通执法体系之外的超凡领域，接触者可能成为重点目标。", "基础规则"
        )
        is False
    )


def test_category_serving_as_summary_is_rejected():
    assert is_chapter_summary_worldview("进度", "主角继续调查。", "章节摘要") is True


def test_content_starting_with_this_chapter_is_rejected():
    assert is_chapter_summary_worldview("调查", "本章执法队搜查老屋。", "基础设定") is True


def test_plot_verb_fallback_requires_non_setting_category():
    # >80 字、命中 ≥3 个叙事动词、非章节指涉、句数 <4（避免先命中叙事复述分支）
    content = (
        "执法队搜查老屋，登上阁楼，指出梁柱与暗格的疑点，把整栋建筑翻了个底朝天，"
        "又把柴房水井逐一排查了一遍，却始终没有找到他们想要的东西。"
        "现场只留下几枚凌乱的脚印和半张烧焦的纸片，除此之外一无所获"
    )
    assert len(content) > 80
    assert is_chapter_summary_worldview("事件", content, "相关背景") is True
    # 同样内容但在设定类目下 → 保留
    assert is_chapter_summary_worldview("事件", content, "历史传说") is False
