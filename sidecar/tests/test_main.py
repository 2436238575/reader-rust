"""--job-file 调试入口：坏输入走结构化 result 错误而不是裸栈。"""

import json

from agent_sidecar.__main__ import main


def test_job_file_with_array_json_returns_structured_error(tmp_path, capsys):
    # 数组 JSON 此前在 frame.get 上裸栈崩溃（AttributeError: 'list' object
    # has no attribute 'get'）——必须以 result_error 帧收场
    path = tmp_path / "job.json"
    path.write_text('["not", "a", "job"]', encoding="utf-8")
    assert main(["--job-file", str(path)]) == 0
    lines = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    assert lines[0]["type"] == "hello"
    assert lines[1]["type"] == "result"
    assert lines[1]["ok"] is False
    assert "JSON 对象" in lines[1]["error"]


def test_job_file_with_string_json_returns_structured_error(tmp_path, capsys):
    path = tmp_path / "job.json"
    path.write_text('"just a string"', encoding="utf-8")
    assert main(["--job-file", str(path)]) == 0
    lines = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    assert lines[-1]["type"] == "result"
    assert lines[-1]["ok"] is False


def test_job_file_with_object_json_still_routes_to_handler(tmp_path, capsys):
    # 对象 JSON 不受影响：bare job 会被包成 run 帧，模型未配置走正常错误路径
    path = tmp_path / "job.json"
    path.write_text(
        json.dumps({"jobId": "debug-1", "kind": "redraw_map", "memory": {}}, ensure_ascii=False),
        encoding="utf-8",
    )
    assert main(["--job-file", str(path)]) == 0
    lines = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    assert lines[0]["type"] == "hello"
    result = lines[-1]
    assert result["type"] == "result"
    assert result["ok"] is False
    assert result["jobId"] == "debug-1"


def test_job_file_missing_file_reports_error(tmp_path, capsys):
    exit_code = main(["--job-file", str(tmp_path / "missing.json")])
    assert exit_code == 1
    assert "读取 job 文件失败" in capsys.readouterr().err
