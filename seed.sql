INSERT INTO "LiteLLM_SpendLogs"
 (request_id, session_id, call_type, api_key, model, model_group, custom_llm_provider, prompt_tokens, completion_tokens,
  total_tokens, spend, "startTime", "endTime", metadata, cache_hit, status)
VALUES
 ('cache-qa-write', 'cache-qa', 'acompletion', 'qa-virtual-key', 'gpt-4o', 'gpt-4o', 'openai', 1000, 50,
  1050, 0.006, now() - interval '2 minutes', now() - interval '2 minutes' + interval '1 second',
  '{"status":"success","additional_usage_values":{"cache_read_input_tokens":0,"cache_creation_input_tokens":800}}', 'False', 'success'),
 ('cache-qa-read', 'cache-qa', 'acompletion', 'qa-virtual-key', 'gpt-4o', 'gpt-4o', 'openai', 1000, 50,
  1050, 0.002, now() - interval '1 minute', now() - interval '1 minute' + interval '1 second',
  '{"status":"success","additional_usage_values":{"prompt_tokens_details":{"cached_tokens":800}}}', 'False', 'success'),
 ('cache-qa-replay', 'cache-qa', 'acompletion', 'qa-virtual-key', 'gpt-4o', 'gpt-4o', 'openai', 1000, 50,
  1050, 0, now() - interval '30 seconds', now() - interval '29 seconds',
  '{"status":"success","additional_usage_values":{"cache_read_input_tokens":800}}', 'True', 'success'),
 ('cache-qa-single', 'cache-single', 'acompletion', 'qa-virtual-key', 'gpt-4o', 'gpt-4o', 'openai', 500, 50,
  550, 0.002, now() - interval '20 seconds', now() - interval '19 seconds',
  '{"status":"success","additional_usage_values":{"cache_read_input_tokens":350,"cache_creation_input_tokens":100}}', 'False', 'success')
ON CONFLICT (request_id) DO NOTHING;
