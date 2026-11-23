import copy
import json
import unittest
from unittest import mock

from scripts import check_openapi


class OpenApiTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.spec = json.loads((check_openapi.ROOT / 'docs/api/openapi.json').read_text(encoding='utf-8'))
        cls.routes = check_openapi.registered_routes(check_openapi.ROOT / 'crates/genaimage-server/src/routes')

    def test_contract_covers_registered_routes_and_resolves_references(self):
        self.assertEqual(check_openapi.validate_spec(self.spec, self.routes), [])

    def test_new_route_is_detected(self):
        errors = check_openapi.validate_spec(self.spec, self.routes | {('/api/v1/new', 'get')})
        self.assertTrue(any('registered route missing' in error for error in errors))

    def test_invalid_refs_and_operation_ids_are_detected(self):
        spec = copy.deepcopy(self.spec)
        spec['components']['schemas']['Bad'] = {'$ref': '#/components/schemas/Missing'}
        spec['paths']['/api/v1/health']['get']['operationId'] = spec['paths']['/api/v1/health']['head']['operationId']
        errors = check_openapi.validate_spec(spec, self.routes)
        self.assertTrue(any('unresolved reference' in error for error in errors))
        self.assertTrue(any('duplicate operationId' in error for error in errors))

    def test_unsupported_merge_fails_closed(self):
        file = mock.Mock()
        file.name = 'mod.rs'
        file.read_text.return_value = 'Router::new().merge(other_router())'
        directory = mock.Mock()
        directory.glob.return_value = [file]
        with self.assertRaisesRegex(ValueError, 'Unsupported router merge'):
            check_openapi.registered_routes(directory)

    def test_unsupported_registration_fails_closed(self):
        file = mock.Mock()
        file.read_text.return_value = 'Router::new().route(dynamic_path, get(handler))'
        directory = mock.Mock()
        directory.glob.return_value = [file]
        with self.assertRaisesRegex(ValueError, 'Unsupported route registration'):
            check_openapi.registered_routes(directory)
