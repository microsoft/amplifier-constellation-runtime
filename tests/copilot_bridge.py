"""Pinned SDK boundary checks; no account or paid inference."""
import asyncio
import importlib.util
import os
from pathlib import Path
from unittest.mock import patch
import unittest

spec = importlib.util.spec_from_file_location('bridge', Path(__file__).parents[1]/'bridges/copilot/provider.py')
bridge = importlib.util.module_from_spec(spec); spec.loader.exec_module(bridge)

class Tests(unittest.IsolatedAsyncioTestCase):
    async def test_capture_deduplicates_first_batch_and_ignores_denied_sdk_retries(self):
        capture = bridge.Capture()
        event = {'type':'assistant.message','data':{'content':'Checking','tool_requests':[{'tool_call_id':'one','name':'clock','arguments':{}},{'tool_call_id':'one','name':'clock','arguments':{}}]}}
        capture.on_event(event)
        capture.on_event({'type':'assistant.message','data':{'tool_requests':[{'tool_call_id':'two','name':'bash','arguments':{}}]}})
        self.assertTrue(capture.done.is_set()); self.assertEqual(len(capture.output),2)
        self.assertEqual(capture.output[1]['call_id'],'one'); self.assertIsNone(capture.error)

    async def test_unlisted_tool_is_captured_for_rust_resolution_not_sdk_execution(self):
        capture = bridge.Capture()
        capture.on_event({'type':'assistant.message','data':{'tool_requests':[{'tool_call_id':'one','name':'bash','arguments':{}}]}})
        self.assertIsNone(capture.error)
        self.assertTrue(capture.done.is_set())
        self.assertTrue(capture.captured)
        self.assertEqual(capture.output,[{'type':'function_call','call_id':'one','name':'bash','arguments':'{}'}])
        capture.on_event({'type':'assistant.message','data':{'tool_requests':[{'tool_call_id':'retry','name':'bash','arguments':{}}]}})
        self.assertEqual(len(capture.output),1)

    async def test_unlisted_tool_keeps_structural_checks(self):
        for call in [{'tool_call_id':'one','name':'bash','arguments':[]},
                     {'tool_call_id':'one','name':'bash','arguments':''},
                     {'tool_call_id':'one','name':'bash','arguments':False},
                     {'tool_call_id':'one','name':'bash','arguments':'{}'},
                     {'tool_call_id':'one','name':'','arguments':{}},
                     {'tool_call_id':'one','name':42,'arguments':{}},
                     {'tool_call_id':42,'name':'bash','arguments':{}}]:
            with self.subTest(call=call):
                capture=bridge.Capture()
                capture.on_event({'type':'assistant.message','data':{'tool_requests':[call]}})
                self.assertTrue(capture.error);self.assertEqual(capture.output,[])

    async def test_sdk_is_denied_every_execution_path_and_destroyed_after_capture(self):
        observed = {}
        class Session:
            async def send(self,prompt):
                observed['prompt']=prompt
                observed['config']['on_event']({'type':'assistant.message','data':{'tool_requests':[{'tool_call_id':'one','name':'unlisted','arguments':{}}]}})
            async def abort(self):observed['aborted']=True
            async def disconnect(self):observed['disconnected']=True
        class Client:
            def __init__(self,**kwargs):observed['client']=kwargs
            async def start(self):pass
            async def create_session(self,**kwargs):observed['config']=kwargs;return Session()
            async def stop(self):observed['stopped']=True
        with patch.dict(os.environ,{'COPILOT_GITHUB_TOKEN':'gho_fixture'}):
            result=await bridge.complete({'protocol_version':1,'operation':'complete','model':'fixture','instructions':'identity','messages':[{'role':'user','content':'check time'}],'tools':[{'name':'clock','description':'clock','parameters':{}}],'max_output_tokens':100},client_factory=Client)
        self.assertFalse(observed['client']['use_logged_in_user']);self.assertEqual(observed['client']['mode'],'empty')
        config=observed['config'];self.assertEqual(config['available_tools'],['clock'])
        self.assertEqual(config['hooks']['on_pre_tool_use']({},None)['permissionDecision'],'deny')
        self.assertEqual(config['on_permission_request'](None).kind,'reject')
        self.assertEqual((await config['tools'][0].handler(None)).result_type,'denied')
        for key in ['enable_config_discovery','enable_session_store','enable_skills','enable_file_hooks','enable_host_git_operations','enable_on_demand_instruction_discovery','enable_session_telemetry']:
            self.assertFalse(config[key])
        self.assertFalse(result['sdk_tools_executed'])
        self.assertEqual(result['output'][0]['name'],'unlisted')
        self.assertTrue(observed['aborted'] and observed['disconnected'] and observed['stopped'])

    async def test_no_token_means_no_ambient_authentication(self):
        with patch.dict(os.environ,{'COPILOT_GITHUB_TOKEN':''}):
            with self.assertRaises(ValueError):
                await bridge.complete({'protocol_version':1,'operation':'models'})

if __name__=='__main__':unittest.main(verbosity=2)
